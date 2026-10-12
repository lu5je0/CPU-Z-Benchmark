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

/// The CPU with the highest maximum frequency that this process is actually
/// allowed to run on.
///
/// CPU-Z pins its single-thread worker to logical CPU 0, which is the right
/// choice on a homogeneous desktop -- but mobile kernels number the fastest
/// cluster *last*. On a Snapdragon 8 Elite Gen 5, cpu0..5 are the 3.63 GHz
/// performance cores and cpu6/7 are the 4.74 GHz primes, so a single-thread
/// pass pinned to CPU 0 silently measures a mid-tier core (582 px/ms instead
/// of the ~760 the prime would give at the same IPC per pixel). Reading
/// cpufreq is the portable way to find the real fastest core.
///
/// The allowed-set check matters: Android's cpuset cgroups can hide the prime
/// cluster from an app entirely, and pinning to an unreachable CPU fails with
/// EINVAL. When cpufreq is not exposed at all this returns `None` and the
/// caller falls back to its default.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn fastest_cpu() -> Option<usize> {
    let mut best: Option<(u64, usize)> = None;
    for entry in std::fs::read_dir("/sys/devices/system/cpu").ok()?.flatten() {
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
        let Ok(text) = std::fs::read_to_string(entry.path().join("cpufreq/cpuinfo_max_freq"))
        else {
            continue;
        };
        let Ok(freq) = text.trim().parse::<u64>() else {
            continue;
        };
        if best.map_or(true, |(best_freq, _)| freq > best_freq) {
            best = Some((freq, cpu));
        }
    }
    best.map(|(_, cpu)| cpu)
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
