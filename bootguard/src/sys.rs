//! Logging, process and /boot helpers.

use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[macro_export]
macro_rules! info { ($($a:tt)*) => { eprintln!("agenticlinux-bootguard: {}", format!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($($a:tt)*) => { eprintln!("agenticlinux-bootguard: WARNING: {}", format!($($a)*)) } }

/// Run a command, killing its process group after `timeout` (Ok(None)).
pub fn run_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
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

/// Run a command and return its stdout; an error if it cannot run or fails.
pub fn capture(prog: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("spawn {prog}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{prog} {} exited with {}", args.join(" "), out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn status_ok(prog: &str, args: &[&str]) -> bool {
    Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
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
        .last()
        .unwrap_or(false)
}

/// Makes /boot writable; restores read-only on drop if it was read-only.
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
            let _ = status_ok("mount", &["-o", "remount,ro", "/boot"]);
        }
    }
}

pub fn is_executable(path: &std::path::Path) -> bool {
    std::fs::metadata(path).map(|m| m.is_file() && m.mode() & 0o111 != 0).unwrap_or(false)
}
