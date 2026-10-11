//! GRUB environment block codec. GRUB reads `grubenv` through its block list,
//! so it must be rewritten in place and keep its size.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::FileExt;

pub const HEADER: &str = "# GRUB Environment Block\n";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: Vec<(String, String)>,
}

impl Env {
    pub fn parse(data: &[u8]) -> Result<Env, String> {
        let text = String::from_utf8_lossy(data);
        let body = text.strip_prefix(HEADER).ok_or("missing GRUB environment block header")?;
        let (mut lines, mut cur, mut esc) = (Vec::new(), String::new(), false);
        for c in body.chars() {
            match (esc, c) {
                (true, c) => {
                    cur.push(c);
                    esc = false;
                }
                (false, '\\') => esc = true,
                (false, '\n') => lines.push(std::mem::take(&mut cur)),
                (false, c) => cur.push(c),
            }
        }
        lines.push(cur);
        let vars = lines
            .iter()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Ok(Env { vars })
    }

    pub fn serialize(&self, size: usize) -> Result<Vec<u8>, String> {
        let mut s = String::from(HEADER);
        for (k, v) in &self.vars {
            s.push_str(&format!("{k}="));
            for c in v.chars() {
                if c == '\\' || c == '\n' {
                    s.push('\\');
                }
                s.push(c);
            }
            s.push('\n');
        }
        if s.len() > size {
            return Err(format!("GRUB environment block too small ({} > {size} bytes)", s.len()));
        }
        let mut b = s.into_bytes();
        b.resize(size, b'#');
        Ok(b)
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    pub fn get_i32(&self, k: &str) -> Option<i32> {
        self.get(k).and_then(|v| v.trim().parse().ok())
    }

    pub fn set(&mut self, k: &str, v: &str) -> Result<(), String> {
        if v.contains(['\n', '\\']) {
            return Err(format!("unsupported character in value for {k}"));
        }
        match self.vars.iter_mut().find(|(n, _)| n == k) {
            Some(e) => e.1 = v.to_string(),
            None => self.vars.push((k.to_string(), v.to_string())),
        }
        Ok(())
    }

    pub fn unset(&mut self, k: &str) {
        self.vars.retain(|(n, _)| n != k);
    }
}

pub fn read_file(path: &str) -> Result<(Env, usize), String> {
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut data))
        .map_err(|e| format!("read {path}: {e}"))?;
    Ok((Env::parse(&data)?, data.len()))
}

/// Rewrite in place (no truncate, no rename) and fsync.
pub fn write_in_place(path: &str, env: &Env, size: usize) -> Result<(), String> {
    let bytes = env.serialize(size)?;
    let f = OpenOptions::new().write(true).open(path).map_err(|e| format!("open {path}: {e}"))?;
    f.write_all_at(&bytes, 0).map_err(|e| format!("write {path}: {e}"))?;
    f.sync_all().map_err(|e| format!("fsync {path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut b = b"# GRUB Environment Block\nboot_success=1\nfallback=1\n".to_vec();
        b.resize(1024, b'#');
        b
    }

    #[test]
    fn parses_real_layout() {
        let e = Env::parse(&sample()).unwrap();
        assert_eq!(e.get("boot_success"), Some("1"));
        assert_eq!(e.get_i32("fallback"), Some(1));
        assert_eq!(e.get("nope"), None);
    }

    #[test]
    fn roundtrip_keeps_size_and_pads_with_hash() {
        let mut e = Env::parse(&sample()).unwrap();
        e.set("bg_counter", "3").unwrap();
        e.set("bg_target", "sha256:abcd").unwrap();
        e.unset("boot_success");
        let out = e.serialize(1024).unwrap();
        assert_eq!((out.len(), out[1023]), (1024, b'#'));
        assert_eq!(Env::parse(&out).unwrap(), e);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Env::parse(b"garbage").is_err());
        let mut e = Env::default();
        assert!(e.set("k", "a\nb").is_err());
        assert!(e.set("k", "a\\b").is_err());
        e.set("a", &"x".repeat(2000)).unwrap();
        assert!(e.serialize(1024).is_err());
    }
}
