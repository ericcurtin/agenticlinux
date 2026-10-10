//! Pure decision logic and the GRUB snippet installer.

pub const MARK_BEGIN: &str = "# BEGIN agenticlinux-bootguard v1";
pub const MARK_END: &str = "# END agenticlinux-bootguard";
pub const SNIPPET: &str = include_str!("../dist/usr/share/agenticlinux-bootguard/grub-custom.cfg");

#[derive(Debug, PartialEq, Eq)]
pub enum Assessment {
    /// Not in a trial.
    Idle,
    /// Counter without a target: clear it.
    Stale,
    /// Running the trial deployment with attempts left.
    TrialInProgress,
    /// Running the trial deployment but its attempts are used up or unknown.
    Exhausted { target: String },
    /// Booted something else: the trial failed and we were sent back.
    FellBack { target: String },
}

pub fn assess(counter: Option<i32>, target: Option<&str>, booted: &str) -> Assessment {
    let Some(t) = target else {
        return if counter.is_some() { Assessment::Stale } else { Assessment::Idle };
    };
    let target = t.to_string();
    if t != booted {
        Assessment::FellBack { target }
    } else if counter.map_or(true, |c| c < 0) {
        // GRUB only counts a present counter, so a missing one can never
        // fall back on its own: fail closed.
        Assessment::Exhausted { target }
    } else {
        Assessment::TrialInProgress
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OnFailure {
    RollbackNow,
    Reboot,
}

/// After a failed health check: reboot for another counted attempt, or roll
/// back now if this was the last one.
pub fn on_check_failure(counter: Option<i32>) -> OnFailure {
    if counter.map_or(false, |c| c > 0) {
        OnFailure::Reboot
    } else {
        OnFailure::RollbackNow
    }
}

/// Replace (or append) our marked block, preserving other content.
pub fn merge_custom_cfg(existing: &str) -> String {
    let block = format!("{MARK_BEGIN}\n{SNIPPET}{MARK_END}\n");
    if let (Some(b), Some(e)) = (existing.find(MARK_BEGIN), existing.find(MARK_END)) {
        if b < e {
            let mut end = e + MARK_END.len();
            if existing[end..].starts_with('\n') {
                end += 1;
            }
            return format!("{}{block}{}", &existing[..b], &existing[end..]);
        }
    }
    let sep = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    format!("{existing}{sep}{block}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assessments() {
        let fell = |t: &str| Assessment::FellBack { target: t.into() };
        let spent = |t: &str| Assessment::Exhausted { target: t.into() };
        assert_eq!(assess(None, None, "A"), Assessment::Idle);
        assert_eq!(assess(Some(2), None, "A"), Assessment::Stale);
        assert_eq!(assess(Some(2), Some("B"), "B"), Assessment::TrialInProgress);
        assert_eq!(assess(Some(0), Some("B"), "B"), Assessment::TrialInProgress);
        assert_eq!(assess(Some(-1), Some("B"), "B"), spent("B"));
        assert_eq!(assess(None, Some("B"), "B"), spent("B"));
        assert_eq!(assess(Some(-1), Some("B"), "A"), fell("B"));
        assert_eq!(assess(Some(2), Some("B"), "A"), fell("B"));
        assert_eq!(assess(None, Some("B"), "A"), fell("B"));
    }

    #[test]
    fn failure_policy() {
        assert_eq!(on_check_failure(Some(2)), OnFailure::Reboot);
        assert_eq!(on_check_failure(Some(1)), OnFailure::Reboot);
        for c in [Some(0), Some(-1), None] {
            assert_eq!(on_check_failure(c), OnFailure::RollbackNow);
        }
    }

    #[test]
    fn custom_cfg_merge_is_idempotent_and_preserves_foreign_content() {
        let once = merge_custom_cfg("set foo=bar\n");
        assert!(once.starts_with("set foo=bar\n") && once.contains("decrement bg_counter"));
        assert_eq!(merge_custom_cfg(&once), once);
        let again = merge_custom_cfg(&format!("{once}menuentry x {{ }}\n"));
        assert!(again.ends_with("menuentry x { }\n"));
        assert_eq!(again.matches(MARK_BEGIN).count(), 1);
        assert_eq!(merge_custom_cfg("").matches(MARK_BEGIN).count(), 1);
    }
}
