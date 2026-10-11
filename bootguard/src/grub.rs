//! GRUB boot counting on ostree: bg_counter/bg_target/fallback in grubenv and
//! a snippet in custom.cfg that decrements the counter on every boot attempt.

use crate::sys::{self, BootRw};
use crate::{grubenv, info, state};

const GRUBENV: &str = "/boot/grub2/grubenv";
const GRUB_CFG: &str = "/boot/grub2/grub.cfg";
const CUSTOM_CFG: &str = "/boot/grub2/custom.cfg";
pub const V_COUNTER: &str = "bg_counter";
pub const V_TARGET: &str = "bg_target";
pub const V_FALLBACK: &str = "fallback";

pub fn read_env() -> Result<grubenv::Env, String> {
    grubenv::read_file(GRUBENV).map(|(e, _)| e)
}

/// Edit grubenv in place, then checkpoint /boot so GRUB sees the change.
fn update_env(f: impl FnOnce(&mut grubenv::Env) -> Result<(), String>) -> Result<(), String> {
    {
        let _rw = BootRw::acquire()?;
        let (mut env, size) = grubenv::read_file(GRUBENV)?;
        f(&mut env)?;
        grubenv::write_in_place(GRUBENV, &env, size)?;
    }
    sys::freeze_thaw("/boot").map_err(|e| format!("could not checkpoint /boot: {e}"))
}

pub fn clear_trial() -> Result<(), String> {
    update_env(|e| {
        [V_COUNTER, V_TARGET, V_FALLBACK].iter().for_each(|k| e.unset(k));
        Ok(())
    })
}

pub fn arm(digest: &str, attempts: u32) -> Result<(), String> {
    setup()?;
    let attempts = attempts.to_string();
    update_env(|e| {
        e.set(V_COUNTER, &attempts)?;
        e.set(V_TARGET, digest)?;
        e.set(V_FALLBACK, "1")
    })
}

/// Does grub.cfg actually `source` a file named custom.cfg (not just mention or
/// comment it)?
pub fn sources_custom_cfg(text: &str) -> bool {
    text.lines().any(|l| {
        let mut w = l.split_whitespace();
        let loads = matches!(w.next(), Some("source" | "configfile" | "."));
        let file = w.next().map(|a| a.trim_matches(['"', '\'']));
        loads && file.is_some_and(|f| f == "custom.cfg" || f.ends_with("/custom.cfg"))
    })
}

pub fn snippet_present() -> bool {
    std::fs::read_to_string(CUSTOM_CFG).is_ok_and(|t| t.contains(state::MARK_BEGIN) && t.contains(state::SNIPPET))
}

pub fn setup() -> Result<(), String> {
    // Unless grub.cfg sources custom.cfg the counter never decrements, so a
    // failing trial would reboot forever: refuse rather than arm.
    let grub_cfg = std::fs::read_to_string(GRUB_CFG).map_err(|e| format!("cannot read {GRUB_CFG}: {e}"))?;
    if !sources_custom_cfg(&grub_cfg) {
        return Err(format!("{GRUB_CFG} does not source custom.cfg, so boot counting cannot work"));
    }
    if snippet_present() {
        return Ok(());
    }
    let merged = state::merge_custom_cfg(&sys::read_or_empty(CUSTOM_CFG)?);
    {
        let _rw = BootRw::acquire()?;
        std::fs::write(CUSTOM_CFG, merged).map_err(|e| format!("write {CUSTOM_CFG}: {e}"))?;
    }
    sys::freeze_thaw("/boot")?;
    info!("installed GRUB boot-counting snippet in {CUSTOM_CFG}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_active_source_line_counts() {
        let real = "if [ -f $prefix/custom.cfg ]; then\n  source $prefix/custom.cfg\nfi\n";
        assert!(sources_custom_cfg(real));
        assert!(sources_custom_cfg("configfile ${prefix}/custom.cfg\n"));
        assert!(!sources_custom_cfg("# source $prefix/custom.cfg\n"));
        assert!(!sources_custom_cfg("if [ -f $prefix/custom.cfg ]; then\nfi\n"));
        assert!(!sources_custom_cfg("echo custom.cfg\n"));
        assert!(!sources_custom_cfg("source $prefix/custom.cfg.bak\n"));
        assert!(!sources_custom_cfg("source $prefix/my-custom.cfg\n"));
        assert!(sources_custom_cfg("  source \"${prefix}/custom.cfg\"\n"));
        assert!(!sources_custom_cfg(""));
    }
}
