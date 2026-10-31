//! C interface to `idealloc`; see `include/idealloc.h`.
use coreba::*;
use std::ffi::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const LIVENESS_INCLUSIVE: u32 = 0;
pub const LIVENESS_INCLUSIVE_EXCLUSIVE: u32 = 1;

pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_THREAD_POOL_ERROR: u32 = 2;
pub const STATUS_INTERNAL_ERROR: u32 = 3;
pub const STATUS_INVALID_PLACEMENT: u32 = 4;

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Buffer {
    pub birth:  u64,
    pub death:  u64,
    pub size:   u64,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct Options {
    pub liveness:       u32,
    pub max_frag:       f64,
    pub max_lives:      u32,
    pub seed:           u64,
    pub alignment:      u64,
    pub num_threads:    u32,
    pub stack_size:     u64,
    pub verbose:        i32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            liveness:       LIVENESS_INCLUSIVE,
            max_frag:       1.0,
            max_lives:      1,
            seed:           0,
            alignment:      0,
            // IREE-sized inputs (up to ~1000 buffers) run fastest on one
            // thread: pool overhead outweighs the parallel boxing.
            num_threads:    1,
            stack_size:     64 << 20,
            verbose:        0,
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn idealloc_options_initialize(options: *mut Options) {
    if !options.is_null() {
        *options = Options::default();
    }
}

#[no_mangle]
pub extern "C" fn idealloc_status_string(status: u32) -> *const c_char {
    let s: &'static [u8] = match status {
        STATUS_OK                   => b"ok\0",
        STATUS_INVALID_ARGUMENT     => b"invalid argument\0",
        STATUS_THREAD_POOL_ERROR    => b"thread pool error\0",
        STATUS_INTERNAL_ERROR       => b"internal error\0",
        STATUS_INVALID_PLACEMENT    => b"invalid placement\0",
        _                           => b"unknown status\0",
    };
    s.as_ptr() as *const c_char
}

#[no_mangle]
pub unsafe extern "C" fn idealloc_place(
    buffers:    *const Buffer,
    count:      usize,
    options:    *const Options,
    offsets:    *mut u64,
    makespan:   *mut u64,
) -> u32 {
    if options.is_null() || makespan.is_null() || (count > 0 && (buffers.is_null() || offsets.is_null())) {
        return STATUS_INVALID_ARGUMENT;
    }
    let bufs = if count > 0 { std::slice::from_raw_parts(buffers, count) } else { &[] };
    let out = if count > 0 { std::slice::from_raw_parts_mut(offsets, count) } else { &mut [] };
    match place(bufs, &*options) {
        Ok((offs, m)) => {
            out.copy_from_slice(&offs);
            *makespan = m;
            STATUS_OK
        },
        Err(status) => status,
    }
}

/// Places `buffers`; returns their offsets and the makespan, or a status.
pub fn place(buffers: &[Buffer], options: &Options) -> Result<(Vec<u64>, u64), u32> {
    let liveness = match options.liveness {
        LIVENESS_INCLUSIVE              => Liveness::Inclusive,
        LIVENESS_INCLUSIVE_EXCLUSIVE    => Liveness::InclusiveExclusive,
        _                               => return Err(STATUS_INVALID_ARGUMENT),
    };
    if !(options.max_frag >= 1.0) || options.max_lives == 0 {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    // Job IDs are u32 and boxes take IDs downward from u32::MAX.
    if buffers.len() >= (u32::MAX / 4) as usize {
        return Err(STATUS_INVALID_ARGUMENT);
    }
    let alignment = options.alignment.max(1);

    // Zero-sized buffers take no space: offset 0, left out of idealloc.
    let mut jobs = vec![];
    let mut sizes = vec![0u64; buffers.len()];
    for (i, b) in buffers.iter().enumerate() {
        let empty = match liveness {
            Liveness::Inclusive             => b.death < b.birth,
            Liveness::InclusiveExclusive    => b.death <= b.birth,
        };
        // (2s, 2e + 2) must fit.
        if empty || b.death > (u64::MAX - 2) / 2 {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        if b.size == 0 {
            continue;
        }
        let size = b.size.checked_next_multiple_of(alignment).ok_or(STATUS_INVALID_ARGUMENT)?;
        sizes[i] = size;
        let (birth, death) = to_exclusive(b.birth as usize, b.death as usize, liveness);
        jobs.push(Job {
            size:               size as usize,
            birth,
            death,
            req_size:           size as usize,
            alignment:          None,
            contents:           None,
            originals_boxed:    0,
            id:                 i as u32,
        });
    }

    let mut offsets = vec![0u64; buffers.len()];
    if !jobs.is_empty() {
        let set = jobset::init(jobs).map_err(|_| STATUS_INVALID_ARGUMENT)?;
        let opts = *options;
        let run = catch_unwind(AssertUnwindSafe(|| {
            pool::with_pool(opts.num_threads as usize, opts.stack_size as usize, move || {
                // The registry holds `Rc`s, which cannot leave the pool.
                let (reg, _) = algo::idealloc(set, opts.max_frag, 0, opts.max_lives, opts.seed, opts.verbose != 0);
                reg.values().map(|pj| (pj.descr.id, pj.offset.get() as u64)).collect::<Vec<_>>()
            })
        }));
        let placed = match run {
            Ok(Ok(v))   => v,
            Ok(Err(_))  => return Err(STATUS_THREAD_POOL_ERROR),
            Err(_)      => return Err(STATUS_INTERNAL_ERROR),
        };
        let mut seen = 0;
        for (id, off) in placed {
            offsets[id as usize] = off;
            seen += 1;
        }
        if seen != sizes.iter().filter(|&&s| s > 0).count() {
            return Err(STATUS_INVALID_PLACEMENT);
        }
    }

    if !is_valid(buffers, &sizes, &offsets, liveness, alignment) {
        return Err(STATUS_INVALID_PLACEMENT);
    }
    let makespan = sizes.iter().zip(&offsets).map(|(s, o)| s + o).max().unwrap_or(0);

    Ok((offsets, makespan))
}

/// Checks the placement against the buffers' own lifetimes (not the
/// converted ones): conflicting buffers must not share bytes, and every
/// offset must be aligned.
fn is_valid(buffers: &[Buffer], sizes: &[u64], offsets: &[u64], liveness: Liveness, alignment: u64) -> bool {
    if offsets.iter().any(|o| o % alignment != 0) {
        return false;
    }
    let conflict = |a: &Buffer, b: &Buffer| match liveness {
        Liveness::Inclusive             => a.birth <= b.death && b.birth <= a.death,
        Liveness::InclusiveExclusive    => a.birth < b.death && b.birth < a.death,
    };
    // Sweep in order of birth, keeping the buffers still live.
    let mut order: Vec<usize> = (0..buffers.len()).filter(|&i| sizes[i] > 0).collect();
    order.sort_by_key(|&i| buffers[i].birth);
    let mut live: Vec<usize> = vec![];
    for &i in &order {
        live.retain(|&j| conflict(&buffers[i], &buffers[j]) || buffers[j].death >= buffers[i].birth);
        for &j in &live {
            if conflict(&buffers[i], &buffers[j])
                && offsets[i] < offsets[j] + sizes[j]
                && offsets[j] < offsets[i] + sizes[i] {
                return false;
            }
        }
        live.push(i);
    }
    true
}
