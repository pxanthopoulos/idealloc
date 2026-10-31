// C interface to idealloc: offline placement of buffers with known sizes and
// lifetimes into one contiguous region (dynamic storage allocation).
//
// Link against libidealloc_ffi.a (and -lpthread -ldl -lm on Linux).
#ifndef IDEALLOC_H_
#define IDEALLOC_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// How buffer lifetimes are read.
typedef enum idealloc_liveness_e {
  // [birth, death]: live at both ends. Buffers [0,1] and [1,2] conflict.
  IDEALLOC_LIVENESS_INCLUSIVE = 0,
  // [birth, death): dead at death. Buffers [0,1) and [1,2) do not conflict.
  IDEALLOC_LIVENESS_INCLUSIVE_EXCLUSIVE = 1,
} idealloc_liveness_t;

typedef enum idealloc_status_e {
  IDEALLOC_STATUS_OK = 0,
  // Bad options or buffers (death < birth, empty interval, overflow, ...).
  IDEALLOC_STATUS_INVALID_ARGUMENT = 1,
  // The worker thread pool could not be created.
  IDEALLOC_STATUS_THREAD_POOL_ERROR = 2,
  // idealloc panicked; the message went to stderr.
  IDEALLOC_STATUS_INTERNAL_ERROR = 3,
  // idealloc returned a placement that failed validation (two conflicting
  // buffers share bytes, or a misaligned offset). Should not happen.
  IDEALLOC_STATUS_INVALID_PLACEMENT = 4,
} idealloc_status_t;

typedef struct idealloc_buffer_t {
  uint64_t birth;
  uint64_t death;
  uint64_t size;  // In bytes. Zero-sized buffers get offset 0.
} idealloc_buffer_t;

typedef struct idealloc_options_t {
  idealloc_liveness_t liveness;
  // Stop as soon as makespan <= max_frag * max load (>= 1.0).
  double max_frag;
  // Tries allowed to beat the bootstrap heuristic (>= 1).
  uint32_t max_lives;
  // Controls all random choices: same input, options and seed give the same
  // placement, whatever the number of threads.
  uint64_t seed;
  // Sizes are rounded up to a multiple of this, so that every offset is one
  // too. 0 or 1: no alignment.
  uint64_t alignment;
  // Worker threads; 0 means one per core.
  uint32_t num_threads;
  // Stack size of each worker thread in bytes; 0 means 1 GiB.
  uint64_t stack_size;
  // Non-zero: print progress and results on stdout.
  int32_t verbose;
} idealloc_options_t;

// Fills |options| with the defaults: inclusive liveness, max_frag 1.0,
// max_lives 1, seed 0, no alignment, 1 thread, 64 MiB stacks, quiet.
void idealloc_options_initialize(idealloc_options_t* options);

// Places |count| buffers. On success, |offsets[i]| is the offset of
// |buffers[i]| and |*makespan| the end of the highest buffer (aligned sizes).
// Safe to call any number of times, from any number of threads.
idealloc_status_t idealloc_place(const idealloc_buffer_t* buffers,
                                 size_t count,
                                 const idealloc_options_t* options,
                                 uint64_t* offsets, uint64_t* makespan);

// A static, human-readable name for |status|.
const char* idealloc_status_string(idealloc_status_t status);

#ifdef __cplusplus
}  // extern "C"
#endif

#endif  // IDEALLOC_H_
