//! KEY=VALUE config: defaults in /usr/lib, overrides in /etc.

use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_PATH: &str = "/usr/lib/agenticlinux-bootguard/bootguard.conf";
pub const ETC_PATH: &str = "/etc/agenticlinux-bootguard/bootguard.conf";

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub preflight: bool,
    pub max_attempts: u32,
    pub trial_timeout: Duration,
    pub check_timeout: Duration,
    pub required_units: Vec<String>,
    pub fail_on_failed_units: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            preflight: true,
            max_attempts: 3,
            trial_timeout: Duration::from_secs(600),
            check_timeout: Duration::from_secs(120),
            required_units: vec![],
            fail_on_failed_units: false,
        }
    }
}

pub fn parse_kv(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            m.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    m
}

impl Config {
    pub fn from_map(m: &HashMap<String, String>) -> Config {
        let mut c = Config::default();
        let b = |k: &str, d: bool| match m.get(k).map(|s| s.as_str()) {
            Some("1") | Some("true") | Some("yes") => true,
            Some("0") | Some("false") | Some("no") => false,
            _ => d,
        };
        let n = |k: &str| m.get(k).and_then(|s| s.parse::<u64>().ok());
        c.enabled = b("ENABLED", c.enabled);
        c.preflight = b("PREFLIGHT", c.preflight);
        c.fail_on_failed_units = b("FAIL_ON_FAILED_UNITS", c.fail_on_failed_units);
        if let Some(v) = n("MAX_ATTEMPTS") {
            c.max_attempts = v.clamp(1, 20) as u32;
        }
        if let Some(v) = n("TRIAL_TIMEOUT") {
            c.trial_timeout = Duration::from_secs(v.max(10));
        }
        if let Some(v) = n("CHECK_TIMEOUT") {
            c.check_timeout = Duration::from_secs(v.max(1));
        }
        if let Some(v) = m.get("REQUIRED_UNITS") {
            c.required_units = v.split_whitespace().map(String::from).collect();
        }
        c
    }

    pub fn load() -> Config {
        let mut m = HashMap::new();
        for p in [DEFAULT_PATH, ETC_PATH] {
            if let Ok(t) = std::fs::read_to_string(p) {
                m.extend(parse_kv(&t));
            }
        }
        Config::from_map(&m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let m = parse_kv("# c\nMAX_ATTEMPTS=5\nTRIAL_TIMEOUT=\"90\"\nREQUIRED_UNITS=\"a.service b.service\"\nENABLED=no\n");
        let c = Config::from_map(&m);
        assert_eq!(c.max_attempts, 5);
        assert_eq!(c.trial_timeout, Duration::from_secs(90));
        assert_eq!(c.required_units, vec!["a.service", "b.service"]);
        assert!(!c.enabled);
        assert!(c.preflight);
        assert_eq!(c.check_timeout, Duration::from_secs(120));
    }

    #[test]
    fn clamps_absurd_values() {
        let c = Config::from_map(&parse_kv("MAX_ATTEMPTS=0\nTRIAL_TIMEOUT=1\n"));
        assert_eq!(c.max_attempts, 1);
        assert_eq!(c.trial_timeout, Duration::from_secs(10));
    }
}
