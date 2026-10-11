//! Logging, process and /boot helpers.

use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Longer than any healthy `bootc status`, shorter than the units that call it.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
/// For `bootc rollback`, mounts and `systemctl`: far above normal, under the units.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[macro_export]
macro_rules! info { ($($a:tt)*) => { eprintln!("agenticlinux-bootguard: {}", format!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($($a:tt)*) => { eprintln!("agenticlinux-bootguard: WARNING: {}", format!($($a)*)) } }

/// Wait for `child`, killing its process group after `timeout` (Ok(None)).
fn wait_deadline(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let start = Instant::now();
    loop {
        if let Some(st) = child.try_wait()? {
            return Ok(Some(st));
        }
        if start.elapsed() >= timeout {
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run a command, killing its process group after `timeout` (Ok(None)).
pub fn run_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    cmd.process_group(0);
    wait_deadline(&mut cmd.spawn()?, timeout)
}

/// Run a command and return its stdout; an error if it cannot run, fails or
/// hangs (a hung `bootc` must not stall the boot service that guards a trial).
pub fn capture(prog: &str, args: &[&str]) -> Result<String, String> {
    capture_within(prog, args, CAPTURE_TIMEOUT)
}

fn capture_within(prog: &str, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("spawn {prog}: {e}"))?;
    let mut out = child.stdout.take().ok_or("no stdout")?;
    // Read concurrently so a large output cannot fill the pipe and stall the child.
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        out.read_to_end(&mut b).map(|_| b)
    });
    let st = wait_deadline(&mut child, timeout)
        .map_err(|e| format!("wait for {prog}: {e}"))?
        .ok_or_else(|| format!("{prog} {} timed out after {}s", args.join(" "), timeout.as_secs()))?;
    if !st.success() {
        return Err(format!("{prog} {} exited with {st}", args.join(" ")));
    }
    let bytes = reader.join().map_err(|_| "reader thread panicked")?.map_err(|e| format!("read {prog}: {e}"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Run `f` up to three times, a second apart: a failed write to /boot is worth
/// retrying before giving up on state a later boot would act on.
pub fn retry<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let mut r = f();
    for _ in 0..2 {
        if r.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
        r = f();
    }
    r
}

/// Read a file; a missing file is empty, any other failure is returned (so we
/// never overwrite content we could not read).
pub fn read_or_empty(path: &str) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("cannot read {path}: {e}")),
    }
}

/// Remove a file; `Ok(false)` if it was not there.
pub fn remove_if_exists(path: &str) -> Result<bool, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("remove {path}: {e}")),
    }
}

/// A digest or id that is safe to store in grubenv or a state file.
pub fn is_safe_id(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || ":._-".contains(c))
}

/// Did the command succeed within `COMMAND_TIMEOUT`? A hung one counts as failed.
pub fn status_ok(prog: &str, args: &[&str]) -> bool {
    status_within(prog, args, COMMAND_TIMEOUT)
}

fn status_within(prog: &str, args: &[&str], timeout: Duration) -> bool {
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null());
    matches!(run_timeout(cmd, timeout), Ok(Some(st)) if st.success())
}

pub fn systemctl(args: &[&str]) -> bool {
    status_ok("systemctl", args)
}

const FIFREEZE: u64 = 0xC004_5877;
const FITHAW: u64 = 0xC004_5878;

/// Freeze and thaw a filesystem to checkpoint its journal. GRUB cannot replay
/// XFS/ext4 journals, so it can read stale metadata for recently changed files.
pub fn freeze_thaw(path: &str) -> Result<(), String> {
    let f = File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let fd = f.as_raw_fd();
    let err = |what: &str| format!("{what} {path}: {}", std::io::Error::last_os_error());
    unsafe {
        libc::sync();
        if libc::ioctl(fd, FIFREEZE as _) != 0 {
            return Err(err("FIFREEZE"));
        }
        for _ in 0..6 {
            if libc::ioctl(fd, FITHAW as _) == 0 {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    Err(err("FITHAW"))
}

/// Is /boot a read-only mount? (the last matching mountinfo line wins)
fn boot_is_ro() -> bool {
    let mi = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    mi.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f.len() > 5 && f[4] == "/boot").then(|| f[5].split(',').any(|o| o == "ro"))
        })
        .next_back()
        .unwrap_or(false)
}

/// Makes /boot writable, restoring read-only on drop. /boot is a bind mount, so
/// read-only needs `bind` (a plain `remount,ro` hits the shared fs: EBUSY).
pub struct BootRw {
    was_ro: bool,
}

impl BootRw {
    pub fn acquire() -> Result<BootRw, String> {
        let was_ro = boot_is_ro();
        if was_ro && !status_ok("mount", &["-o", "remount,rw", "/boot"]) {
            return Err("cannot remount /boot read-write".into());
        }
        Ok(BootRw { was_ro })
    }
}

impl Drop for BootRw {
    fn drop(&mut self) {
        if self.was_ro {
            let _ = status_ok("mount", &["-o", "remount,bind,ro", "/boot"]);
        }
    }
}

pub fn is_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_returns_output_and_bounds_a_hang() {
        let t = Duration::from_secs(5);
        assert_eq!(capture_within("echo", &["hi"], t), Ok("hi\n".into()));
        let big = capture_within("head", &["-c", "300000", "/dev/zero"], t).unwrap();
        assert_eq!(big.len(), 300_000, "output larger than the pipe buffer");
        assert!(capture_within("false", &[], t).unwrap_err().contains("exited"));
        let start = Instant::now();
        let e = capture_within("sleep", &["30"], Duration::from_millis(500)).unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(start.elapsed() < Duration::from_secs(5), "must not wait for the child");
    }

    #[test]
    fn status_is_bounded() {
        let t = Duration::from_secs(5);
        assert!(status_within("true", &[], t));
        assert!(!status_within("false", &[], t));
        assert!(!status_within("/nonexistent/prog", &[], t));
        let start = Instant::now();
        assert!(!status_within("sleep", &["30"], Duration::from_millis(500)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn safe_ids() {
        for ok in ["sha256:ab12", "ab12", "a.b_c-d"] {
            assert!(is_safe_id(ok), "{ok}");
        }
        for bad in ["", "a b", "a\nb", "a=b", "a\\b", "a\"b"] {
            assert!(!is_safe_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn read_or_empty_only_forgives_a_missing_file() {
        let p = std::env::temp_dir().join(format!("bg-read-{}", std::process::id()));
        let p = p.to_str().unwrap();
        assert_eq!(read_or_empty(p), Ok(String::new()));
        std::fs::write(p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_or_empty(p).unwrap_err().contains("cannot read"));
        std::fs::write(p, "ok").unwrap();
        assert_eq!(read_or_empty(p), Ok("ok".into()));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn remove_if_exists_reports_whether_it_removed() {
        let p = std::env::temp_dir().join(format!("bg-rm-{}", std::process::id()));
        let p = p.to_str().unwrap();
        std::fs::write(p, "x").unwrap();
        assert_eq!(remove_if_exists(p), Ok(true));
        assert_eq!(remove_if_exists(p), Ok(false));
    }
}
