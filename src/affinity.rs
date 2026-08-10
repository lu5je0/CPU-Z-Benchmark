//! CPU affinity helpers, mirroring CPU-Z's per-worker thread affinity
//! (SetThreadAffinityMask / SetThreadGroupAffinity). On Linux we use
//! sched_setaffinity so each worker is pinned to its own logical CPU,
//! which is critical for the multi-threaded score (CPU-Z pins every worker
//! thread to a distinct core).

#[cfg(target_os = "linux")]
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

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn pin_thread_to_cpu(_cpu: usize) {}
