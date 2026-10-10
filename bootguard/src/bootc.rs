//! Interface to `bootc` and `ostree`.

use crate::sys::{capture, status_ok};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default, Clone)]
struct Raw {
    status: Option<Host>,
}
#[derive(Debug, Deserialize, Default, Clone)]
struct Host {
    booted: Option<Entry>,
    staged: Option<Entry>,
    rollback: Option<Entry>,
}
#[derive(Debug, Deserialize, Default, Clone)]
struct Entry {
    image: Option<Img>,
    ostree: Option<Ostree>,
}
#[derive(Debug, Deserialize, Default, Clone)]
struct Img {
    #[serde(rename = "imageDigest")]
    image_digest: Option<String>,
}
#[derive(Debug, Deserialize, Default, Clone)]
struct Ostree {
    checksum: Option<String>,
    #[serde(rename = "deploySerial")]
    deploy_serial: Option<u32>,
    stateroot: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Deployment {
    pub digest: String,
    pub checksum: String,
    pub serial: u32,
    pub stateroot: String,
}

impl Deployment {
    pub fn dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "/sysroot/ostree/deploy/{}/deploy/{}.{}",
            self.stateroot, self.checksum, self.serial
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub booted: Option<Deployment>,
    pub staged: Option<Deployment>,
    pub rollback: Option<Deployment>,
}

fn conv(e: &Option<Entry>) -> Option<Deployment> {
    let e = e.as_ref()?;
    let digest = e.image.as_ref()?.image_digest.clone()?;
    let o = e.ostree.clone().unwrap_or_default();
    Some(Deployment {
        digest,
        checksum: o.checksum.unwrap_or_default(),
        serial: o.deploy_serial.unwrap_or(0),
        stateroot: o.stateroot.unwrap_or_else(|| "default".into()),
    })
}

pub fn parse_status(json: &str) -> Result<Status, String> {
    let raw: Raw = serde_json::from_str(json).map_err(|e| format!("parse bootc status: {e}"))?;
    let h = raw.status.unwrap_or_default();
    Ok(Status { booted: conv(&h.booted), staged: conv(&h.staged), rollback: conv(&h.rollback) })
}

pub fn status() -> Result<Status, String> {
    parse_status(&capture("bootc", &["status", "--json"])?)
}

pub fn rollback() -> Result<(), String> {
    if status_ok("bootc", &["rollback"]) {
        Ok(())
    } else {
        Err("bootc rollback failed".into())
    }
}

/// Is the booted deployment first in the boot order? A non-booted first entry
/// ("(pending)") boots next; a "(staged)" one is not in the order yet.
pub fn booted_is_default() -> Result<bool, String> {
    let out = capture("ostree", &["admin", "status"])?;
    Ok(parse_booted_is_default(&out))
}

pub fn parse_booted_is_default(out: &str) -> bool {
    for l in out.lines() {
        let t = l.trim_start_matches(' ');
        let booted = t.starts_with("* ");
        let t = t.trim_start_matches("* ");
        let mut it = t.split_whitespace();
        let (Some(_root), Some(id)) = (it.next(), it.next()) else { continue };
        if !id.contains('.') || l.contains("(staged)") {
            continue;
        }
        return booted;
    }
    true
}

/// Drop the staged deployment so `ostree-finalize-staged` has nothing to do.
pub fn unstage() -> Result<(), String> {
    match std::fs::remove_file("/run/ostree/staged-deployment") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("remove staged-deployment: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        let j = r#"{"status":{"booted":{"image":{"imageDigest":"sha256:aa"},"ostree":{"checksum":"c1","deploySerial":1,"stateroot":"default"}},
                    "staged":{"image":{"imageDigest":"sha256:bb"},"ostree":{"checksum":"c2","deploySerial":0,"stateroot":"default"}},"rollback":null}}"#;
        let s = parse_status(j).unwrap();
        assert_eq!(s.booted.unwrap().digest, "sha256:aa");
        let st = s.staged.unwrap();
        assert_eq!(st.dir().to_str().unwrap(), "/sysroot/ostree/deploy/default/deploy/c2.0");
        assert!(s.rollback.is_none());
    }

    #[test]
    fn default_order() {
        assert!(parse_booted_is_default("* default abc.0\n    origin: x\n  default def.0 (rollback)\n"));
        assert!(!parse_booted_is_default("  default def.0\n    origin: x\n* default abc.1 (rollback)\n"));
        assert!(!parse_booted_is_default("  default new.0 (pending)\n* default abc.0\n"));
        assert!(parse_booted_is_default("  default new.0 (staged)\n* default abc.0\n"));
    }
}
