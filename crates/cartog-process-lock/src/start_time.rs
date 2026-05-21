//! Cross-platform process start-time lookup.
//!
//! Used in combination with the PID to detect PID reuse: a stale PID file
//! may point to a recycled PID assigned to an unrelated process. Comparing
//! start times disambiguates the two.
//!
//! Returned values are OS-native and not portable across machines, but they
//! are stable across multiple reads of the same live PID — which is all we
//! need for our same-process check.
//!
//! Linux PID-namespace caveat: a container whose `/proc` is overlaid (e.g.
//! the container sees its own PID 1 but the host PID is different) may get
//! `None` back from `process_start_time` for its own PID. Callers fall back
//! to `is_alive` semantics, so PID-reuse detection just degrades to plain
//! liveness: safe, not exact.

/// Look up the start time of a running process. Returns `None` if the
/// process is gone, inaccessible, or the platform is unsupported.
#[cfg(target_os = "linux")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    // /proc/<pid>/stat field 22 is `starttime`, the time the process started
    // after system boot in clock ticks. Stable for the life of the process.
    // The file is tab/space-delimited but field 2 (comm) is parenthesised
    // and may contain spaces — we slice from the last ')' to skip it.
    let raw = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = raw.rfind(')')?;
    // After ')' the layout is " <state> <ppid> ... " with field numbering
    // restarting at 3 (state). starttime is field 22 → 22 - 3 = 19 spaces in.
    let tail = raw.get(after_comm + 2..)?;
    tail.split_ascii_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "macos")]
pub fn process_start_time(pid: u32) -> Option<u64> {
    use libc::{c_int, c_void};

    // proc_pidinfo(PROC_PIDTBSDINFO) returns a struct whose `pbi_start_tvsec`
    // field is the start time in seconds since the epoch. The proc_bsdinfo
    // struct isn't in the `libc` crate, so we define just enough of it here
    // to read the field we need (it's stable ABI per <sys/proc_info.h>).
    const PROC_PIDTBSDINFO: c_int = 3;

    // Layout per /usr/include/sys/proc_info.h (proc_bsdinfo).
    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [u8; 16],
        pbi_name: [u8; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffer_size: c_int,
        ) -> c_int;
    }

    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    // proc_pidinfo returns 0 (and we return None) when the caller's UID
    // doesn't match the target. Treated identically to "process gone";
    // callers fall back to is_alive() semantics.
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::uninit();
    let size = std::mem::size_of::<ProcBsdInfo>() as c_int;
    // SAFETY: proc_pidinfo is a stable libproc entry point; we pass an
    // uninitialized buffer of the documented size and check the return
    // value before reading any field.
    let bytes_written = unsafe {
        proc_pidinfo(
            pid as c_int,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr() as *mut c_void,
            size,
        )
    };
    if bytes_written != size {
        return None;
    }
    // SAFETY: proc_pidinfo wrote `size` bytes into `info`. We read only
    // `pbi_start_tvsec` via addr_of! rather than assume_init on the whole
    // struct, so if Apple appends fields to proc_bsdinfo in a future SDK
    // (changing total size) we still tolerate the change as long as the
    // offset of pbi_start_tvsec stays stable.
    let start = unsafe { std::ptr::addr_of!((*info.as_ptr()).pbi_start_tvsec).read_unaligned() };
    Some(start)
}

#[cfg(windows)]
pub fn process_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    // SAFETY: OpenProcess validates the pid and returns a non-null handle
    // for live, accessible processes.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = creation;
    let mut kernel = creation;
    let mut user = creation;
    // SAFETY: handle is valid (just opened); all out-pointers point to
    // local FILETIME storage we own.
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    // SAFETY: handle was returned by a successful OpenProcess.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    Some(((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn process_start_time(_pid: u32) -> Option<u64> {
    // Unsupported platform: returning None disables the PID-reuse check.
    // Callers fall back to `is_alive` semantics — same as before this module.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_has_a_start_time() {
        let pid = std::process::id();
        let st = process_start_time(pid);
        assert!(
            st.is_some(),
            "process_start_time must return Some(_) for our own PID"
        );
    }

    #[test]
    fn self_start_time_is_stable() {
        // Two reads of our own start time must agree; the value is set at
        // exec time and never changes.
        let pid = std::process::id();
        let a = process_start_time(pid).expect("self has a start_time");
        std::thread::sleep(std::time::Duration::from_millis(50));
        let b = process_start_time(pid).expect("self has a start_time");
        assert_eq!(a, b, "start_time must be stable across reads");
    }

    #[test]
    fn dead_pid_returns_none() {
        // Same sentinel as is_alive's clearly-dead test.
        assert!(process_start_time(4_194_304).is_none());
    }

    #[test]
    fn pid_zero_returns_none() {
        assert!(process_start_time(0).is_none());
    }
}
