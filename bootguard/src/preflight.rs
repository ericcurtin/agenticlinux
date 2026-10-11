//! Static checks of a staged deployment, run before the reboot: kernel,
//! initramfs, root= kargs and fstab.

use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Finding {
    pub fatal: bool,
    pub msg: String,
}

impl Finding {
    pub fn tag(&self) -> &'static str {
        if self.fatal { "FAIL" } else { "note" }
    }
}

fn fatal(m: impl Into<String>) -> Finding {
    Finding { fatal: true, msg: m.into() }
}
fn note(m: impl Into<String>) -> Finding {
    Finding { fatal: false, msg: m.into() }
}

fn le<const N: usize>(b: &[u8], o: usize) -> Option<[u8; N]> {
    b.get(o..o + N)?.try_into().ok()
}
fn u16le(b: &[u8], o: usize) -> Option<u16> {
    le(b, o).map(u16::from_le_bytes)
}
fn u32le(b: &[u8], o: usize) -> Option<u32> {
    le(b, o).map(u32::from_le_bytes)
}

/// Validate a kernel image from its header and total size.
pub fn check_kernel(head: &[u8], size: u64) -> Result<(), String> {
    let bz = head.get(0x202..0x206) == Some(b"HdrS");
    if !head.starts_with(b"MZ") {
        return if bz {
            check_bzimage(head, size)
        } else if head.get(56..60) == Some(b"ARM\x64") {
            check_arm64_image(size)
        } else {
            Err("not a recognizable kernel image (bad magic)".into())
        };
    }
    let pe = u32le(head, 0x3c).ok_or("truncated DOS header")? as usize;
    if head.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("MZ header without a valid PE signature".into());
    }
    let nsec = u16le(head, pe + 6).ok_or("truncated COFF header")? as usize;
    let sec0 = pe + 24 + u16le(head, pe + 20).ok_or("truncated COFF header")? as usize;
    let mut end = 0u64;
    for i in 0..nsec {
        let o = sec0 + i * 40;
        let raw = u32le(head, o + 16).ok_or("PE section table beyond header")? as u64;
        let ptr = u32le(head, o + 20).ok_or("PE section table beyond header")? as u64;
        end = end.max(ptr + raw);
    }
    if size < end {
        return Err(format!("truncated: file is {size} bytes but PE sections extend to {end}"));
    }
    if head.get(4..8) == Some(b"zimg") {
        let need = u32le(head, 8).unwrap_or(0) as u64 + u32le(head, 12).unwrap_or(0) as u64;
        if size < need {
            return Err(format!("truncated: zboot payload needs {need} bytes, file is {size}"));
        }
    }
    if bz {
        check_bzimage(head, size)?;
    }
    Ok(())
}

/// Raw arm64 `Image`: only the 64-byte header is checked. `image_size` is the
/// effective memory size (including BSS), not the file length, so it is no
/// basis for a truncation check.
fn check_arm64_image(size: u64) -> Result<(), String> {
    if size < 64 {
        return Err(format!("truncated arm64 Image: file is {size} bytes"));
    }
    Ok(())
}

fn check_bzimage(head: &[u8], size: u64) -> Result<(), String> {
    let sects = match *head.get(0x1f1).ok_or("truncated bzImage header")? {
        0 => 4,
        n => n as u64,
    };
    let sys = u32le(head, 0x1f4).ok_or("truncated bzImage header")? as u64;
    let need = (sects + 1) * 512 + sys * 16;
    if size < need {
        return Err(format!("truncated bzImage: needs {need} bytes, file is {size}"));
    }
    Ok(())
}

fn is_init(name: &str) -> bool {
    let n = name.trim_start_matches("./").trim_start_matches('/');
    ["init", "sbin/init", "usr/sbin/init", "usr/lib/systemd/systemd", "lib/systemd/systemd"].contains(&n)
}

/// Does this start a cpio entry? `Some(true)` for the CRC format (070702),
/// whose header carries a checksum of the file data.
fn cpio_has_checksum(magic: &[u8]) -> Option<bool> {
    match magic {
        b"070701" => Some(false),
        b"070702" => Some(true),
        _ => None,
    }
}

/// Sum of `n` bytes at `off`, modulo 2^32 (the cpio CRC format's checksum).
fn byte_sum(f: &File, mut off: u64, mut n: u64) -> std::io::Result<u32> {
    let mut buf = [0u8; 1 << 16];
    let mut sum = 0u32;
    while n > 0 {
        let k = n.min(buf.len() as u64) as usize;
        f.read_exact_at(&mut buf[..k], off)?;
        sum = buf[..k].iter().fold(sum, |s, &b| s.wrapping_add(b as u32));
        (off, n) = (off + k as u64, n - k as u64);
    }
    Ok(sum)
}

/// Skip uncompressed "early" cpio archives (CPU microcode) at the start of an
/// initramfs. Returns the offset after them, and whether they held an init.
pub fn initramfs_payload_offset(f: &File, len: u64) -> Result<(u64, bool), String> {
    let (mut o, mut has_init) = (0u64, false);
    loop {
        let pad_end = (o + (1 << 20)).min(len);
        let mut z = [0u8; 1];
        while o < pad_end && f.read_exact_at(&mut z, o).is_ok() && z[0] == 0 {
            o += 1;
        }
        let mut magic = [0u8; 6];
        if f.read_exact_at(&mut magic, o).is_err() || cpio_has_checksum(&magic).is_none() {
            return Ok((o, has_init));
        }
        loop {
            let mut h = [0u8; 110];
            f.read_exact_at(&mut h, o).map_err(|e| format!("early cpio header: {e}"))?;
            let hex = |a: usize| {
                std::str::from_utf8(&h[a..a + 8]).ok().and_then(|s| u64::from_str_radix(s, 16).ok())
            };
            let crc = cpio_has_checksum(&h[..6]).ok_or("bad cpio magic")?;
            let (Some(fsz), Some(nsz), Some(sum)) = (hex(54), hex(94), hex(102)) else {
                return Err("bad cpio header".into());
            };
            if nsz == 0 || nsz > 4096 || o + 110 + nsz > len {
                return Err(format!("implausible cpio name size {nsz}"));
            }
            let mut name = vec![0u8; nsz as usize];
            f.read_exact_at(&mut name, o + 110).map_err(|e| format!("early cpio name: {e}"))?;
            let name = String::from_utf8_lossy(&name[..nsz as usize - 1]).into_owned();
            let a4 = |x: u64| (x + 3) & !3;
            let data = a4(o + 110 + nsz);
            o = a4(data + fsz);
            if o > len {
                return Err("early cpio runs past end of file".into());
            }
            if crc && byte_sum(f, data, fsz).map_err(|e| format!("early cpio data: {e}"))? as u64 != sum {
                return Err(format!("checksum mismatch in cpio entry {name}"));
            }
            if name == "TRAILER!!!" {
                break;
            }
            has_init |= is_init(&name);
            if o == len {
                return Err("early cpio ends without a trailer".into());
            }
        }
    }
}

pub fn detect_compression(b: &[u8]) -> Option<(&'static str, &'static [&'static str])> {
    const MAGIC: [(&[u8], &str, &[&str]); 5] = [
        (&[0x28, 0xB5, 0x2F, 0xFD], "zstd", &["-tq"]),
        (&[0x1f, 0x8b], "gzip", &["-tq"]),
        (&[0xFD, b'7', b'z', b'X', b'Z', 0], "xz", &["-tq"]),
        (&[0x04, 0x22, 0x4D, 0x18], "lz4", &["-tq"]),
        (b"BZh", "bzip2", &["-tq"]),
    ];
    MAGIC.iter().find(|(m, ..)| b.starts_with(m)).map(|&(_, p, a)| (p, a))
}

pub fn check_initramfs(path: &Path) -> Vec<Finding> {
    let mut f = match File::open(path) {
        Ok(f) => f,
        Err(e) => return vec![fatal(format!("cannot open {}: {e}", path.display()))],
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < 1024 {
        return vec![fatal(format!("initramfs is only {len} bytes"))];
    }
    let (off, has_init) = match initramfs_payload_offset(&f, len) {
        Ok(r) => r,
        Err(e) => return vec![fatal(format!("initramfs early cpio is damaged: {e}"))],
    };
    if off >= len {
        // only cpio archives: a plain initramfs is fine if it has an init
        return if has_init { vec![] } else { vec![fatal("initramfs holds only early archives and no init")] };
    }
    let mut head = [0u8; 8];
    let _ = f.read_exact_at(&mut head, off);
    let Some((prog, args)) = detect_compression(&head) else {
        return vec![fatal("initramfs payload has no known compression magic")];
    };
    let _ = f.seek(SeekFrom::Start(off));
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::from(f)).stdout(Stdio::null()).stderr(Stdio::null());
    match crate::sys::run_timeout(cmd, Duration::from_secs(180)) {
        Ok(Some(st)) if st.success() => vec![],
        Ok(Some(_)) => vec![fatal(format!("initramfs ({prog}) failed its integrity test; it is corrupt or truncated"))],
        Ok(None) => vec![fatal("initramfs integrity test timed out")],
        Err(e) => vec![note(format!("cannot verify initramfs with `{prog}`: {e}"))],
    }
}

#[derive(Deserialize, Default)]
struct KargsFile {
    #[serde(default)]
    kargs: Vec<String>,
    #[serde(default, rename = "match-architectures")]
    match_architectures: Vec<String>,
}

/// Kernel arguments from a bootc kargs.d file that apply to `arch`.
pub fn parse_kargs_toml(text: &str, arch: &str) -> Result<Vec<String>, String> {
    let f: KargsFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let applies = f.match_architectures.is_empty() || f.match_architectures.iter().any(|a| a == arch);
    Ok(if applies { f.kargs } else { vec![] })
}

/// Map a `root=`/fstab spec to a device node we can test for existence.
pub fn spec_to_dev(spec: &str) -> Option<String> {
    let by = |dir: &str, v: &str| Some(format!("/dev/disk/{dir}/{}", v.trim_matches('"')));
    match spec.split_once('=') {
        Some(("UUID", v)) => by("by-uuid", v),
        Some(("LABEL", v)) => by("by-label", v),
        Some(("PARTUUID", v)) => by("by-partuuid", v),
        Some(("PARTLABEL", v)) => by("by-partlabel", v),
        _ => spec.starts_with("/dev/").then(|| spec.to_string()),
    }
}

pub fn check_kargs(kargs: &[String], exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    kargs
        .iter()
        .filter_map(|k| k.strip_prefix("root=").and_then(spec_to_dev).map(|dev| (k, dev)))
        .filter(|(_, dev)| !exists(dev))
        .map(|(k, dev)| format!("kernel argument `{k}` refers to {dev}, which does not exist"))
        .collect()
}

pub fn check_fstab(text: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    const SKIP_OPTS: [&str; 4] = ["nofail", "noauto", "bind", "rbind"];
    const SKIP_FS: [&str; 10] = ["nfs", "nfs4", "cifs", "smb3", "9p", "virtiofs", "tmpfs", "overlay", "none", "bpf"];
    let mut errs = Vec::new();
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 2 {
            continue;
        }
        let fstype = f.get(2).copied().unwrap_or("auto");
        let opts = f.get(3).copied().unwrap_or("defaults");
        if opts.split(',').any(|o| SKIP_OPTS.contains(&o)) || SKIP_FS.contains(&fstype) || fstype.starts_with("fuse") {
            continue;
        }
        if let Some(dev) = spec_to_dev(f[0]).filter(|d| !exists(d)) {
            errs.push(format!("fstab entry `{l}`: {dev} does not exist and the entry lacks `nofail`"));
        }
    }
    errs
}

fn check_tool(deploy: &Path, tool: &str) -> Option<Finding> {
    let p = deploy.join(tool.trim_start_matches('/'));
    (!crate::sys::is_executable(&p))
        .then(|| fatal(format!("new image has no executable {tool}; a trial boot could never be confirmed")))
}

fn read_head(path: &Path, n: usize) -> Result<(Vec<u8>, u64), String> {
    let mut f = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut buf = vec![0u8; n.min(size as usize)];
    f.read_exact(&mut buf).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok((buf, size))
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Run every static check against an ostree deployment directory.
pub fn run(deploy: &Path, tool: &str) -> Vec<Finding> {
    if !deploy.is_dir() {
        return vec![fatal(format!("staged deployment {} not found", deploy.display()))];
    }
    let exists = |p: &str| Path::new(p).exists();
    let mut out = Vec::new();

    let mut found_kernel = false;
    for d in subdirs(&deploy.join("usr/lib/modules")) {
        let vm = d.join("vmlinuz");
        if !vm.exists() {
            continue;
        }
        found_kernel = true;
        if let Err(e) = read_head(&vm, 65536).and_then(|(h, s)| check_kernel(&h, s)) {
            out.push(fatal(format!("kernel {}: {e}", vm.display())));
        }
        let ir = d.join("initramfs.img");
        if ir.exists() {
            out.extend(check_initramfs(&ir));
        } else {
            out.push(fatal(format!("{} is missing", ir.display())));
        }
    }
    if !found_kernel {
        out.push(fatal("no kernel found under usr/lib/modules"));
    }

    if let Ok(rd) = std::fs::read_dir(deploy.join("usr/lib/bootc/kargs.d")) {
        for p in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            match parse_kargs_toml(&text, std::env::consts::ARCH) {
                Ok(k) => out.extend(check_kargs(&k, &exists).into_iter().map(|m| fatal(format!("{}: {m}", p.display())))),
                Err(e) => out.push(fatal(format!("{}: invalid TOML: {e}", p.display()))),
            }
        }
    }

    // Staged /etc is merged again at finalize, so check every source.
    let mut seen = HashSet::new();
    for p in [deploy.join("etc/fstab"), PathBuf::from("/etc/fstab"), deploy.join("usr/etc/fstab")] {
        for m in std::fs::read_to_string(&p).iter().flat_map(|t| check_fstab(t, &exists)) {
            if seen.insert(m.clone()) {
                out.push(fatal(format!("{}: {m}", p.display())));
            }
        }
    }

    out.extend(check_tool(deploy, tool));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str, data: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("bg-{name}-{}", std::process::id()));
        File::create(&p).unwrap().write_all(data).unwrap();
        p
    }

    fn pe(raw: u32, ptr: u32) -> Vec<u8> {
        let mut h = vec![0u8; 0x400];
        h[..2].copy_from_slice(b"MZ");
        h[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        h[0x94..0x96].copy_from_slice(&0xf0u16.to_le_bytes());
        let s = 0x80 + 24 + 0xf0;
        h[s + 16..s + 20].copy_from_slice(&raw.to_le_bytes());
        h[s + 20..s + 24].copy_from_slice(&ptr.to_le_bytes());
        h
    }

    fn cpio_entry(name: &str, data: &[u8], nsz: usize) -> Vec<u8> {
        let mut v = format!(
            "070701{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
            0, 0o100644, 0, 0, 1, 0, data.len(), 0, 0, 0, 0, nsz, 0
        )
        .into_bytes();
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v.extend_from_slice(data);
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    fn cpio(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b: Vec<u8> = entries.iter().flat_map(|(n, d)| cpio_entry(n, d, n.len() + 1)).collect();
        b.extend(cpio_entry("TRAILER!!!", b"", 11));
        b
    }

    #[test]
    fn kernel_pe_ok_truncated_and_garbage() {
        let h = pe(1000, 0x400);
        assert!(check_kernel(&h, 0x400 + 1000).is_ok());
        assert!(check_kernel(&h, 0x400 + 999).unwrap_err().contains("truncated"));
        assert!(check_kernel(&[0xAB; 4096], 4096).unwrap_err().contains("bad magic"));
        assert!(check_kernel(b"MZ", 2).is_err());
    }

    #[test]
    fn kernel_zboot_payload_truncation() {
        let mut h = pe(100, 0x400);
        h[4..8].copy_from_slice(b"zimg");
        h[8..12].copy_from_slice(&0x400u32.to_le_bytes());
        h[12..16].copy_from_slice(&5000u32.to_le_bytes());
        assert!(check_kernel(&h, 0x400 + 5000).is_ok());
        assert!(check_kernel(&h, 0x400 + 4000).unwrap_err().contains("truncated"));
    }

    #[test]
    fn kernel_bzimage() {
        let mut h = vec![0u8; 0x400];
        h[0x202..0x206].copy_from_slice(b"HdrS");
        h[0x1f1] = 3;
        h[0x1f4..0x1f8].copy_from_slice(&100u32.to_le_bytes());
        let need = 4 * 512 + 100 * 16;
        assert!(check_kernel(&h, need).is_ok());
        assert!(check_kernel(&h, need - 1).is_err());
    }

    #[test]
    fn kernel_raw_arm64() {
        let mut h = vec![0u8; 64];
        h[16..24].copy_from_slice(&(1u64 << 25).to_le_bytes());
        h[56..60].copy_from_slice(b"ARM\x64");
        assert!(check_kernel(&h, 64).is_ok(), "image_size is not a file length");
        assert!(check_kernel(&h[..60], 60).unwrap_err().contains("truncated"));
    }

    #[test]
    fn compression_magic() {
        assert_eq!(detect_compression(&[0x28, 0xB5, 0x2F, 0xFD, 0]).unwrap().0, "zstd");
        assert_eq!(detect_compression(&[0x1f, 0x8b, 8]).unwrap().0, "gzip");
        assert!(detect_compression(&[1, 2, 3, 4, 5, 6]).is_none());
        assert!(detect_compression(b"070701").is_none());
    }

    #[test]
    fn early_cpio_is_skipped() {
        let mut blob = cpio(&[("kernel/x86/microcode/a.bin", b"MICROCODE")]);
        let early = blob.len() as u64;
        blob.extend_from_slice(&[0x28, 0xB5, 0x2F, 0xFD, 1, 2, 3, 4]);
        blob.resize(4096, 0);
        let p = tmp("early", &blob);
        let f = File::open(&p).unwrap();
        assert_eq!(initramfs_payload_offset(&f, 4096).unwrap(), (early, false));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn plain_cpio_needs_an_init() {
        let mut ok = cpio(&[("init", b"#!/bin/sh")]);
        ok.resize(2048, 0);
        let mut early_only = cpio(&[("kernel/x86/microcode/a.bin", b"M")]);
        early_only.resize(2048, 0);
        let (a, b) = (tmp("plain", &ok), tmp("earlyonly", &early_only));
        assert!(check_initramfs(&a).is_empty());
        assert!(check_initramfs(&b).iter().any(|f| f.fatal && f.msg.contains("no init")));
        std::fs::remove_file(a).ok();
        std::fs::remove_file(b).ok();
    }

    #[test]
    fn cpio_crc_checksum_and_trailer_bounds() {
        let data = [b'x'; 1100];
        let crc = |chk: u32| {
            let mut e = cpio_entry("init", &data, 5);
            e[..6].copy_from_slice(b"070702");
            e[102..110].copy_from_slice(format!("{chk:08x}").as_bytes());
            e.extend(cpio_entry("TRAILER!!!", b"", 11));
            e
        };
        let sum = 1100 * u32::from(b'x');
        let mut big_trailer = cpio(&[("init", &data)]);
        let t = cpio_entry("init", &data, 5).len();
        big_trailer[t + 54..t + 62].copy_from_slice(b"00010000");
        let no_trailer = cpio_entry("init", &data, 5);
        let files = [
            tmp("crc-ok", &crc(sum)),
            tmp("crc-bad", &crc(sum + 1)),
            tmp("bigtrailer", &big_trailer),
            tmp("notrailer", &no_trailer),
        ];
        assert!(check_initramfs(&files[0]).is_empty());
        for (p, why) in files[1..].iter().zip(["checksum mismatch", "past end", "without a trailer"]) {
            let r = check_initramfs(p);
            assert!(r.iter().any(|f| f.fatal && f.msg.contains("damaged") && f.msg.contains(why)), "{why}: {r:?}");
        }
        files.iter().for_each(|p| std::fs::remove_file(p).unwrap_or(()));
    }

    #[test]
    fn oversized_cpio_name_is_rejected_without_allocating() {
        let mut blob = cpio_entry("x", b"", 0xffff_ffff);
        blob.resize(4096, 0);
        let p = tmp("bigname", &blob);
        let r = check_initramfs(&p);
        assert!(r.iter().any(|f| f.fatal && f.msg.contains("name size")), "{r:?}");
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn garbage_initramfs_rejected() {
        let p = tmp("garbage", &vec![0xA7u8; 8192]);
        assert!(check_initramfs(&p).iter().any(|f| f.fatal && f.msg.contains("magic")));
        std::fs::remove_file(p).ok();
    }

    #[test]
    fn kargs_toml_is_parsed_structurally() {
        let t = "# kargs = [\"root=UUID=comment\"]\nkargs = [\"quiet\", 'root=UUID=dead-beef', \"x=\\\"y\\\"\"]\n";
        let k = parse_kargs_toml(t, "aarch64").unwrap();
        assert_eq!(k, vec!["quiet", "root=UUID=dead-beef", "x=\"y\""]);
        assert_eq!(check_kargs(&k, &|_| false).len(), 1);
        assert!(check_kargs(&k, &|_| true).is_empty());
        assert!(check_kargs(&["root=live:http://x".into()], &|_| false).is_empty());
        let multi = "kargs = [\n \"a\",\n \"b\"\n]\nmatch-architectures = [\"x86_64\"]\n";
        assert_eq!(parse_kargs_toml(multi, "x86_64").unwrap(), vec!["a", "b"]);
        assert!(parse_kargs_toml(multi, "aarch64").unwrap().is_empty());
        assert!(parse_kargs_toml("kargs = [", "x86_64").is_err());
    }

    #[test]
    fn fstab_rules() {
        let t = "UUID=1111 /gbtest xfs defaults 0 0\nUUID=2222 /ok xfs defaults,nofail 0 0\n\
                 //srv/share /mnt cifs defaults 0 0\n/dev/nope /x ext4 defaults 0 0\nUUID=3333 /y xfs noauto 0 0\n\
                 # UUID=4444 / xfs defaults\ntmpfs /tmp tmpfs defaults 0 0\n";
        let errs = check_fstab(t, &|_| false);
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs[0].contains("1111"));
        assert!(check_fstab(t, &|_| true).is_empty());
    }

    #[test]
    fn tool_must_be_an_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("bg-tool-{}", std::process::id()));
        std::fs::create_dir_all(d.join("usr/bin")).unwrap();
        let tool = "/usr/bin/t";
        assert!(check_tool(&d, tool).is_some());
        std::fs::write(d.join("usr/bin/t"), "x").unwrap();
        assert!(check_tool(&d, tool).is_some());
        std::fs::set_permissions(d.join("usr/bin/t"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_tool(&d, tool).is_none());
        std::fs::create_dir_all(d.join("usr/bin/dir")).unwrap();
        assert!(check_tool(&d, "/usr/bin/dir").is_some());
        std::fs::remove_dir_all(d).ok();
    }
}
