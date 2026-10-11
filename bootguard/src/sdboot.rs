//! systemd-boot boot counting on bootc's composefs backend. systemd-boot renames
//! `x+3.conf` to `x+2-1.conf` on each attempt; at `+0` the entry is bad and
//! sorts last, so the previous deployment boots. `arm` counts the staged entry,
//! a healthy boot strips the counter. We rename ourselves: Fedora's SELinux
//! policy denies `systemd-bless-boot.service` (init_t) renames on the FAT ESP.

use crate::bootc::Deployment;
use crate::sys::{self, BootRw};
use std::path::{Path, PathBuf};

pub const ENTRIES: &str = "/boot/loader/entries";
pub const STAGED: &str = "/boot/loader/entries.staged";
const STAGED_FLAG: &str = "/run/composefs/staged-deployment";
const EFIVARS: &str = "/sys/firmware/efi/efivars";
const LOADER_GUID: &str = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f";
const FEATURE_BOOT_COUNTING: u64 = 1 << 4;
const TRIAL: &str = "/var/lib/agenticlinux-bootguard/trial";
const STAGED_MNT: &str = "/run/agenticlinux-bootguard/staged";

/// `(tries left, tries done)` from `name+3-1.conf`; `name+3.conf` has 0 done.
pub fn parse_counter(file: &str) -> Option<(u32, u32)> {
    let (_, c) = file.strip_suffix(".conf")?.rsplit_once('+')?;
    let (left, done) = c.split_once('-').unwrap_or((c, "0"));
    Some((left.parse().ok()?, done.parse().ok()?))
}

/// The file name without its counter.
pub fn base_name(file: &str) -> String {
    match (file.strip_suffix(".conf"), parse_counter(file)) {
        (Some(stem), Some(_)) => format!("{}.conf", stem.rsplit_once('+').map_or(stem, |s| s.0)),
        _ => file.to_string(),
    }
}

pub fn with_tries(file: &str, left: u32, done: Option<u32>) -> String {
    let stem = base_name(file);
    let stem = stem.strip_suffix(".conf").unwrap_or(&stem);
    match done {
        Some(d) => format!("{stem}+{left}-{d}.conf"),
        None => format!("{stem}+{left}.conf"),
    }
}

/// UTF-16LE string from an EFI variable (4 attribute bytes first).
pub fn efivar_string(raw: &[u8]) -> Option<String> {
    let units: Vec<u16> = raw.get(4..)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&u| u != 0).collect();
    String::from_utf16(&units).ok()
}

fn efivar(name: &str) -> Option<Vec<u8>> {
    std::fs::read(format!("{EFIVARS}/{name}-{LOADER_GUID}")).ok()
}

/// The composefs id an entry boots: `options ... composefs=?<id> ...`.
pub fn entry_verity(text: &str) -> Option<String> {
    let opts = text.lines().find_map(|l| l.strip_prefix("options"))?;
    let v = opts.split_whitespace().find_map(|t| t.strip_prefix("composefs="))?;
    Some(v.trim_start_matches('?').to_string())
}

/// Refuse unless we were booted by a systemd-boot that counts boots.
pub fn setup() -> Result<(), String> {
    let raw = efivar("LoaderFeatures").ok_or("not booted by systemd-boot (no LoaderFeatures)")?;
    let bytes: [u8; 8] = raw.get(4..12).and_then(|b| b.try_into().ok()).ok_or("malformed LoaderFeatures")?;
    if u64::from_le_bytes(bytes) & FEATURE_BOOT_COUNTING == 0 {
        return Err("this systemd-boot does not support boot counting".into());
    }
    Ok(())
}

/// The counted entry we booted from: `(file name, tries left)`.
pub fn selected() -> Option<(String, u32)> {
    let path = efivar_string(&efivar("LoaderBootCountPath")?)?;
    let file = path.rsplit(['\\', '/']).next()?.to_string();
    parse_counter(&file).map(|(left, _)| (file, left))
}

/// `selected`, if its entry still has the counter (it is not blessed yet).
pub fn pending() -> Option<(String, u32)> {
    selected().filter(|(file, _)| Path::new(ENTRIES).join(file).exists())
}

/// File name of the entry in `dir` that boots composefs deployment `verity`.
fn find_entry(dir: &str, verity: &str) -> Result<Option<String>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot read {dir}: {e}"))?;
    for e in rd {
        let e = e.map_err(|e| format!("cannot read {dir}: {e}"))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".conf") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).map_err(|e| format!("cannot read {dir}/{name}: {e}"))?;
        if entry_verity(&text).as_deref() == Some(verity) {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

fn rename(dir: &str, from: &str, to: &str) -> Result<(), String> {
    if from == to {
        return Ok(());
    }
    let _rw = BootRw::acquire()?;
    std::fs::rename(Path::new(dir).join(from), Path::new(dir).join(to)).map_err(|e| format!("rename {from} to {to}: {e}"))?;
    unsafe { libc::sync() }; // FAT has no journal to checkpoint, only data to flush
    Ok(())
}

/// `(image digest, composefs id)` of the trial we armed.
pub fn parse_trial(text: &str) -> Option<(String, String)> {
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).map(String::from);
    Some((get("target")?, get("verity")?))
}

pub fn load_trial() -> Option<(String, String)> {
    parse_trial(&std::fs::read_to_string(TRIAL).ok()?)
}

pub fn clear_trial() -> Result<(), String> {
    sys::remove_if_exists(TRIAL).map(|_| ())
}

fn save_trial(target: &str, verity: &str) -> Result<(), String> {
    if !sys::is_safe_id(target) || !sys::is_safe_id(verity) {
        return Err(format!("unexpected deployment id {target:?} / {verity:?}"));
    }
    let tmp = format!("{TRIAL}.tmp");
    std::fs::create_dir_all(Path::new(TRIAL).parent().unwrap_or(Path::new("/")))
        .and_then(|()| std::fs::write(&tmp, format!("target={target}\nverity={verity}\n")))
        .and_then(|()| std::fs::rename(&tmp, TRIAL))
        .map_err(|e| format!("write {TRIAL}: {e}"))
}

/// Give the staged entry `attempts` boot tries; bootc's finalize installs it.
pub fn arm(staged: &Deployment, attempts: u32) -> Result<(), String> {
    setup()?; // a counter systemd-boot cannot decrement would never roll back
    let verity = staged.verity.as_deref().ok_or("staged deployment has no composefs id")?;
    let file = find_entry(STAGED, verity)?.ok_or_else(|| format!("no staged boot entry for {verity}"))?;
    save_trial(&staged.digest, verity)?;
    rename(STAGED, &file, &with_tries(&file, attempts, None))
}

/// Strip the counter from the entry we booted: the trial succeeded.
pub fn bless() -> Result<(), String> {
    pending().map_or(Ok(()), |(file, _)| rename(ENTRIES, &file, &base_name(&file)))
}

/// Mark deployment `verity`'s entry bad (0 tries left) so it is never the
/// default. `Ok(false)` if it has no entry.
pub fn mark_bad(verity: &str) -> Result<bool, String> {
    let Some(file) = find_entry(ENTRIES, verity)? else { return Ok(false) };
    match parse_counter(&file) {
        Some((0, _)) => Ok(true),
        c => rename(ENTRIES, &file, &with_tries(&file, 0, c.map(|(_, done)| done))).map(|()| true),
    }
}

/// Drop the staged deployment so bootc's finalize has nothing to do.
/// Returns whether one was staged.
pub fn unstage() -> Result<bool, String> {
    let was = sys::remove_if_exists(STAGED_FLAG)?;
    if Path::new(STAGED).exists() {
        let _rw = BootRw::acquire()?;
        std::fs::remove_dir_all(STAGED).map_err(|e| format!("remove {STAGED}: {e}"))?;
        unsafe { libc::sync() };
    }
    Ok(was)
}

/// A staged composefs image mounted read-only, unmounted on drop.
pub struct StagedTree(PathBuf);

impl StagedTree {
    pub fn mount(verity: &str) -> Result<StagedTree, String> {
        if verity.is_empty() || !verity.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("unexpected composefs id {verity:?}"));
        }
        std::fs::create_dir_all(STAGED_MNT).map_err(|e| format!("create {STAGED_MNT}: {e}"))?;
        let image = format!("/sysroot/composefs/images/{verity}");
        let ok = sys::status_ok("mount.composefs", &[&image, STAGED_MNT, "-o", "basedir=/sysroot/composefs/objects"]);
        if !ok {
            return Err(format!("cannot mount the staged image {image}"));
        }
        Ok(StagedTree(PathBuf::from(STAGED_MNT)))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for StagedTree {
    fn drop(&mut self) {
        let _ = sys::status_ok("umount", &[STAGED_MNT]);
    }
}

/// The merged /etc bootc keeps for a deployment (carries the admin's fstab).
pub fn state_etc(verity: &str) -> PathBuf {
    PathBuf::from(format!("/sysroot/state/deploy/{verity}/etc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_parse_and_rewrite() {
        assert_eq!(parse_counter("a-44-1+3.conf"), Some((3, 0)));
        assert_eq!(parse_counter("a-44-1+2-1.conf"), Some((2, 1)));
        assert_eq!(parse_counter("a-44-1+0-3.conf"), Some((0, 3)));
        for none in ["a-44-1.conf", "a+.conf", "a+x.conf", "a+3-.conf", "a+3", "a+3-1-2.conf"] {
            assert_eq!(parse_counter(none), None, "{none}");
        }
        assert_eq!(base_name("a-44-1+2-1.conf"), "a-44-1.conf");
        assert_eq!(base_name("a-44-1.conf"), "a-44-1.conf");
        assert_eq!(base_name("c++-1+3.conf"), "c++-1.conf");
        assert_eq!(with_tries("a-44-1.conf", 3, None), "a-44-1+3.conf");
        assert_eq!(with_tries("a-44-1+2-1.conf", 0, Some(1)), "a-44-1+0-1.conf");
        assert_eq!(parse_counter(&with_tries("x.conf", 3, None)), Some((3, 0)));
    }

    #[test]
    fn find_entry_reports_read_errors_instead_of_no_match() {
        let d = std::env::temp_dir().join(format!("bg-entries-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let dir = d.to_str().unwrap();
        std::fs::write(d.join("a+2-1.conf"), "title a\noptions rw composefs=?aa\n").unwrap();
        std::fs::write(d.join("README"), [0xff]).unwrap(); // not a .conf: ignored
        assert_eq!(find_entry(dir, "aa").unwrap().as_deref(), Some("a+2-1.conf"));
        assert_eq!(find_entry(dir, "bb").unwrap(), None);
        std::fs::write(d.join("b.conf"), [0xff, 0xfe]).unwrap();
        assert!(find_entry(dir, "bb").unwrap_err().contains("cannot read"), "a bad entry is an error, not no match");
        assert!(find_entry("/nonexistent/entries", "aa").is_err());
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn efivar_utf16() {
        let mut raw = vec![7, 0, 0, 0];
        raw.extend("\\loader\\entries\\a+2-1.conf".encode_utf16().flat_map(|u| u.to_le_bytes()));
        raw.extend([0, 0, 0xff, 0xff]);
        assert_eq!(efivar_string(&raw).unwrap(), "\\loader\\entries\\a+2-1.conf");
        assert_eq!(efivar_string(&[7, 0]), None);
    }

    #[test]
    fn entry_verity_from_options() {
        let t = "title x\nversion 44\nsort-key k\noptions rw console=ttyAMA0 composefs=?ab12 panic=5\n";
        assert_eq!(entry_verity(t).as_deref(), Some("ab12"));
        assert_eq!(entry_verity("options composefs=cd34\n").as_deref(), Some("cd34"));
        assert_eq!(entry_verity("options rw\n"), None);
        assert_eq!(entry_verity("title x\n"), None);
    }

    #[test]
    fn trial_file_needs_both_fields() {
        assert_eq!(parse_trial("target=sha256:aa\nverity=bb\n"), Some(("sha256:aa".into(), "bb".into())));
        assert_eq!(parse_trial("target=x\n"), None);
        assert_eq!(parse_trial(""), None);
    }
}
