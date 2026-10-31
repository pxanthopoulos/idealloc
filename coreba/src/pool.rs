//! Thread pools for running [`crate::algo::idealloc`].
//!
//! `idealloc` used to build rayon's *global* pool itself, which panics the
//! second time it is called in a process. Callers now pick the pool: each
//! (thread count, stack size) pair gets one pool, built on first use and
//! reused afterwards, so `idealloc` may be called any number of times, from
//! any number of threads.
use crate::helpe::*;
use std::sync::OnceLock;

/// The core algorithm is recursive. This is the stack size the original
/// `idealloc` binary gave each of its threads; large inputs (hundreds of
/// thousands of jobs) need it.
pub const DEFAULT_STACK_SIZE: usize = 1 << 30;

type PoolKey = (usize, usize);
static POOLS: OnceLock<Mutex<HashMap<PoolKey, Arc<rayon::ThreadPool>>>> = OnceLock::new();

/// Runs `f` on a pool with `num_threads` threads (0: one per core) of
/// `stack_size` bytes each (0: [`DEFAULT_STACK_SIZE`]). The threads only
/// reserve their stacks; memory is committed as the recursion uses it.
pub fn with_pool<R, F>(num_threads: usize, stack_size: usize, f: F) -> Result<R, rayon::ThreadPoolBuildError>
where
    R: Send,
    F: FnOnce() -> R + Send,
{
    let stack_size = if stack_size == 0 { DEFAULT_STACK_SIZE } else { stack_size };
    let pool = {
        let mut pools = POOLS.get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match pools.get(&(num_threads, stack_size)) {
            Some(p) => p.clone(),
            None    => {
                let p = Arc::new(rayon::ThreadPoolBuilder::new()
                    .num_threads(num_threads)
                    .stack_size(stack_size)
                    .thread_name(|i| format!("idealloc-{i}"))
                    .build()?);
                pools.insert((num_threads, stack_size), p.clone());
                p
            }
        }
    };

    Ok(pool.install(f))
}
