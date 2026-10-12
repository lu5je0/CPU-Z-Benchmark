//! CPU affinity helpers, mirroring CPU-Z's per-worker thread affinity
//! (SetThreadAffinityMask / SetThreadGroupAffinity). On Linux we use
//! sched_setaffinity so each worker is pinned to its own logical CPU,
//! which is critical for the multi-threaded score (CPU-Z pins every worker
//! thread to a distinct core).
//!
//! Android is a separate `target_os` in Rust but is bionic/Linux underneath,
//! and the libc crate exposes the same `sched_setaffinity` + `CPU_SET` API
//! there, so Termux gets the identical per-core pinning. (Pinning matters even
//! more on big.LITTLE SoCs: without it the scheduler can park several workers
//! on the same little core.)
//!
//! Picking *which* CPU is a second problem: CPU-Z pins its single worker to
//! logical CPU 0, which is right on a homogeneous desktop but wrong on a
//! mobile SoC, where the kernel numbers the fastest cluster last. See
//! [`cpus_fastest_first`].

#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn pin_thread_to_cpu(cpu: usize) {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        let ret = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
        if ret != 0 {
            eprintln!("warning: sched_setaffinity({}) failed", cpu);
        }
    }
}

#[cfg(target_os = "windows")]
pub fn pin_thread_to_cpu(cpu: usize) {
    unsafe {
        let mask: usize = 1usize << (cpu % 64);
        // Windows: pin to one logical processor of group 0.
        extern "system" {
            fn SetThreadAffinityMask(hThread: *mut std::ffi::c_void, dwThreadAffinityMask: usize) -> usize;
        }
        extern "system" {
            fn GetCurrentThread() -> *mut std::ffi::c_void;
        }
        SetThreadAffinityMask(GetCurrentThread(), mask);
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "windows"
)))]
pub fn pin_thread_to_cpu(_cpu: usize) {}

/// Reachable logical CPUs, fastest first.
///
/// CPU-Z pins its workers to CPU 0..n. That is right on a desktop, where the
/// cores are equivalent, but mobile kernels number the fastest cluster *last*:
/// on a Snapdragon 8 Elite Gen 5 cpu0..5 are the 3.63 GHz performance cores and
/// cpu6/7 are the 4.74 GHz primes, so a single-thread pass left on CPU 0
/// silently measures a mid-tier core (582 px/ms instead of the ~760 the prime
/// turns in at the same per-pixel cost). Ordering by `cpufreq` is the portable
/// way to find the real fastest cores, and every worker pool uses this list so
/// the multi-thread pass spreads over the same set.
///
/// The affinity-mask filter matters: Android's cpuset cgroups can hide the
/// prime cluster from an app entirely, and pinning to an unreachable CPU fails
/// with EINVAL. When cpufreq is not exposed (containers, some VMs) the plain
/// `0..n` order is kept so behaviour is unchanged there. Empty on platforms
/// without affinity control, which leaves the caller unpinned.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn cpus_fastest_first() -> Vec<usize> {
    let mut cpus: Vec<(u64, usize)> = Vec::new();
    if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
        for entry in dir.flatten() {
            let name = entry.file_name();
            let Some(cpu) = name
                .to_string_lossy()
                .strip_prefix("cpu")
                .and_then(|n| n.parse::<usize>().ok())
            else {
                continue;
            };
            if !cpu_is_allowed(cpu) {
                continue;
            }
            let path = entry.path().join("cpufreq/cpuinfo_max_freq");
            let Ok(text) = std::fs::read_to_string(path) else {
                continue;
            };
            let Ok(freq) = text.trim().parse::<u64>() else {
                continue;
            };
            cpus.push((freq, cpu));
        }
    }
    if cpus.is_empty() {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        return (0..n).filter(|&cpu| cpu_is_allowed(cpu)).collect();
    }
    // Fastest first, ties broken by CPU index so the order is deterministic.
    cpus.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    cpus.into_iter().map(|(_, cpu)| cpu).collect()
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn cpus_fastest_first() -> Vec<usize> {
    Vec::new()
}

/// The single fastest reachable CPU, i.e. the one the 1T pass should use.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn fastest_cpu() -> Option<usize> {
    cpus_fastest_first().into_iter().next()
}

/// Is `cpu` inside this process' affinity mask? Assumed allowed when the
/// kernel refuses to tell us.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn cpu_is_allowed(cpu: usize) -> bool {
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return true;
        }
        libc::CPU_ISSET(cpu, &set)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CPU list must be non-empty, free of duplicates and reachable, both
    /// on a machine that exposes cpufreq and on one that does not (CI runners
    /// typically fall into the `0..n` fallback).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn cpus_are_reachable_and_distinct() {
        let cpus = cpus_fastest_first();
        assert!(!cpus.is_empty(), "at least the calling CPU must be usable");
        for &cpu in &cpus {
            assert!(cpu_is_allowed(cpu), "cpu {cpu} is outside the affinity mask");
        }
        let mut sorted = cpus.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), cpus.len(), "duplicate CPU in {cpus:?}");
    }
}
