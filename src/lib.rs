pub mod affinity;
pub mod bench;
pub mod kernel;
pub mod noise;
pub mod noise1901;
/// x86_64-only: the 19.01 AVX2 engine uses `std::arch::x86_64` intrinsics.
/// On other architectures `Engine::V1901Avx2` falls back to the 19.01 SSE
/// engine (see `kernel::pixel` / `bench::worker_loop`).
#[cfg(target_arch = "x86_64")]
pub mod noise_avx2;
pub mod table;
