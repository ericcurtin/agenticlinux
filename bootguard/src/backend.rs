//! The two supported boot stacks: ostree + GRUB, and composefs + systemd-boot.
//! Everything bootloader-specific lives behind this type.

use crate::bootc::{self, Deployment, Status};
use crate::{grub, sdboot, sys};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Grub,
    SdBoot,
}

impl Backend {
    pub fn detect(st: &Status) -> Result<Backend, String> {
        let b = st.booted.as_ref().ok_or("no booted deployment")?;
        match (&b.verity, b.loader.as_deref()) {
            (None, _) => Ok(Backend::Grub),
            (Some(_), Some("systemd")) => Ok(Backend::SdBoot),
            (Some(_), l) => Err(format!("unsupported bootloader {} on the composefs backend", l.unwrap_or("(unknown)"))),
        }
    }

    /// Check that boot counting can work here, installing what it needs.
    pub fn setup(self) -> Result<(), String> {
        match self {
            Backend::Grub => grub::setup(),
            Backend::SdBoot => sdboot::setup(),
        }
    }

    /// `(attempt counter, target image digest)` of the trial in progress. The
    /// counter is the tries left after this boot's own attempt.
    pub fn trial(self, booted: &Deployment) -> Result<(Option<i32>, Option<String>), String> {
        match self {
            Backend::Grub => {
                let env = grub::read_env()?;
                Ok((env.get_i32(grub::V_COUNTER), env.get(grub::V_TARGET).map(String::from)))
            }
            Backend::SdBoot => {
                let sel = sdboot::selected();
                // A counted entry without our record is still ours to judge.
                let target = sdboot::load_trial().map(|t| t.0).or_else(|| sel.as_ref().map(|_| booted.digest.clone()));
                Ok((sel.map(|(_, left)| left as i32), target))
            }
        }
    }

    /// Start a trial for the staged deployment.
    pub fn arm(self, staged: &Deployment, attempts: u32) -> Result<(), String> {
        if !sys::is_safe_id(&staged.digest) {
            return Err(format!("unexpected image digest {:?}", staged.digest));
        }
        match self {
            Backend::Grub => grub::arm(&staged.digest, attempts),
            Backend::SdBoot => sdboot::arm(staged, attempts),
        }
    }

    /// Undo a failed `arm`.
    pub fn discard_arm(self) {
        let _ = self.forget();
    }

    /// Stop tracking the trial, leaving the boot entries as they are.
    pub fn forget(self) -> Result<(), String> {
        match self {
            Backend::Grub => grub::clear_trial(),
            Backend::SdBoot => sdboot::clear_trial(),
        }
    }

    /// The trial succeeded (or is abandoned): stop tracking it and, for
    /// systemd-boot, strip the counter. The record goes first so a failure in
    /// between leaves a counted entry we can bless on the next boot.
    pub fn confirm(self) -> Result<(), String> {
        self.forget()?;
        match self {
            Backend::Grub => Ok(()),
            Backend::SdBoot => sdboot::bless(),
        }
    }

    /// Running the failed deployment: make the previous one the default.
    pub fn roll_back_from(self, failed: &Deployment) -> Result<(), String> {
        match self {
            // Idempotent: if a half-finished rollback already demoted us, a second
            // `bootc rollback` would swap the failed deployment back to default.
            Backend::Grub if !bootc::booted_is_default()? => Ok(()),
            Backend::Grub => bootc::rollback(),
            Backend::SdBoot => match sdboot::mark_bad(failed.verity.as_deref().ok_or("no composefs id")?)? {
                true => Ok(()),
                false => Err("no boot entry found for the failed deployment".into()),
            },
        }
    }

    /// Running the previous deployment after a fallback: make it permanent.
    pub fn make_fallback_permanent(self) -> Result<(), String> {
        match self {
            Backend::Grub => {
                if bootc::booted_is_default()? {
                    return Ok(());
                }
                crate::info!("booted deployment is not the default; running bootc rollback");
                bootc::rollback()?;
                if bootc::booted_is_default()? {
                    Ok(())
                } else {
                    Err("bootc rollback ran but the booted deployment is still not the default".into())
                }
            }
            Backend::SdBoot => sdboot::load_trial().map_or(Ok(()), |(_, verity)| sdboot::mark_bad(&verity).map(|_| ())),
        }
    }

    /// Drop the staged deployment; returns whether one was staged.
    pub fn unstage(self) -> Result<bool, String> {
        match self {
            Backend::Grub => bootc::unstage(),
            Backend::SdBoot => sdboot::unstage(),
        }
    }

    /// Run `f` on the staged deployment's file tree and any extra `/etc` dirs.
    pub fn with_staged_tree<T>(self, dep: &Deployment, f: impl FnOnce(&Path, &[PathBuf]) -> T) -> Result<T, String> {
        match self {
            Backend::Grub => Ok(f(&dep.dir(), &[])),
            Backend::SdBoot => {
                let verity = dep.verity.as_deref().ok_or("staged deployment has no composefs id")?;
                let tree = sdboot::StagedTree::mount(verity)?;
                Ok(f(tree.path(), &[sdboot::state_etc(verity)]))
            }
        }
    }

    /// Lines for `status`.
    pub fn report(self) -> Result<Vec<String>, String> {
        match self {
            Backend::Grub => {
                let env = grub::read_env()?;
                let mut v: Vec<String> = [grub::V_COUNTER, grub::V_TARGET, grub::V_FALLBACK]
                    .iter()
                    .map(|k| format!("{k}: {}", env.get(k).unwrap_or("-")))
                    .collect();
                v.push(format!("grub snippet installed: {}", grub::snippet_present()));
                Ok(v)
            }
            Backend::SdBoot => Ok(vec![
                format!("counted entry: {}", sdboot::pending().map_or("-".into(), |(f, _)| f)),
                format!("trial: {}", sdboot::load_trial().map_or("-".into(), |t| t.0)),
                format!("boot counting supported: {}", sdboot::setup().is_ok()),
            ]),
        }
    }
}

/// Drop a staged deployment of either stack, for when the stack is unknown: the
/// two markers differ, so removing both is harmless on either.
pub fn unstage_any() -> Result<bool, String> {
    let (ostree, composefs) = (bootc::unstage(), sdboot::unstage());
    Ok(ostree? | composefs?)
}

/// Is a trial armed, judged from bootloader state alone (bootc may be broken)?
pub fn trial_armed_without_bootc() -> bool {
    grub::read_env().is_ok_and(|e| e.get(grub::V_COUNTER).is_some()) || sdboot::pending().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(json: &str) -> Status {
        bootc::parse_status(json).unwrap()
    }

    #[test]
    fn detects_the_backend_from_bootc_status() {
        let ostree = st(r#"{"status":{"booted":{"image":{"imageDigest":"sha256:a"},"ostree":{"checksum":"c","deploySerial":0}}}}"#);
        let sd = st(r#"{"status":{"booted":{"image":{"imageDigest":"sha256:a"},"ostree":null,"composefs":{"verity":"ab","bootloader":"systemd"}}}}"#);
        let grub_cfs = st(r#"{"status":{"booted":{"image":{"imageDigest":"sha256:a"},"composefs":{"verity":"ab","bootloader":"grub"}}}}"#);
        assert_eq!(Backend::detect(&ostree), Ok(Backend::Grub));
        assert_eq!(Backend::detect(&sd), Ok(Backend::SdBoot));
        assert!(Backend::detect(&grub_cfs).unwrap_err().contains("unsupported bootloader grub"));
        assert!(Backend::detect(&st(r#"{"status":{}}"#)).is_err());
    }
}
