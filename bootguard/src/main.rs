//! Automatic rollback for failed `bootc upgrade --apply`. See README.md.

mod bootc;
mod config;
mod grubenv;
mod preflight;
mod state;
mod sys;

use config::Config;
use state::{Assessment, OnFailure};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sys::{BootRw, Res};

const TOOL: &str = "/usr/bin/agenticlinux-bootguard";
const GRUBENV: &str = "/boot/grub2/grubenv";
const CUSTOM_CFG: &str = "/boot/grub2/custom.cfg";
const MOTD: &str = "/etc/motd.d/agenticlinux-bootguard";
const V_COUNTER: &str = "bg_counter";
const V_TARGET: &str = "bg_target";
const V_FALLBACK: &str = "fallback";

fn run_file(name: &str) -> String {
    format!("/run/agenticlinux-bootguard/{name}")
}
fn state_file(name: &str) -> String {
    format!("/var/lib/agenticlinux-bootguard/{name}")
}

// ---- grubenv -------------------------------------------------------------

fn read_env() -> Result<grubenv::Env, String> {
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

fn clear_trial() -> Result<(), String> {
    update_env(|e| {
        [V_COUNTER, V_TARGET, V_FALLBACK].iter().for_each(|k| e.unset(k));
        Ok(())
    })
}

// ---- records and notices -------------------------------------------------

fn record(file: &str, line: &str) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(state_file(""));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(state_file(file)) {
        let _ = writeln!(f, "{line}");
    }
}

fn rejected_digests() -> Vec<String> {
    let t = std::fs::read_to_string(state_file("rejected")).unwrap_or_default();
    t.lines().filter_map(|l| l.split_whitespace().next().map(String::from)).collect()
}

fn notify(msg: &str) {
    let _ = std::fs::create_dir_all("/etc/motd.d");
    let _ = std::fs::write(MOTD, format!("{msg}\n"));
    record("last-event.txt", msg);
    warn!("{msg}");
}

fn reject(digest: &str, reason: &str, applied: bool) {
    if !rejected_digests().iter().any(|d| d == digest) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        record("rejected", &format!("{digest} {now} {reason}"));
    }
    let what = if applied { "rejected and rolled back" } else { "rejected before it was applied" };
    notify(&format!("The update {digest} was {what}: {reason}"));
}

// ---- GRUB snippet --------------------------------------------------------

fn snippet_present() -> bool {
    std::fs::read_to_string(CUSTOM_CFG).is_ok_and(|t| t.contains(state::MARK_BEGIN) && t.contains(state::SNIPPET))
}

fn setup() -> Result<(), String> {
    let grub_cfg = std::fs::read_to_string("/boot/grub2/grub.cfg").unwrap_or_default();
    if !grub_cfg.is_empty() && !grub_cfg.contains("custom.cfg") {
        warn!("grub.cfg does not source custom.cfg; boot counting will not work");
    }
    if snippet_present() {
        return Ok(());
    }
    let merged = state::merge_custom_cfg(&std::fs::read_to_string(CUSTOM_CFG).unwrap_or_default());
    {
        let _rw = BootRw::acquire()?;
        std::fs::write(CUSTOM_CFG, merged).map_err(|e| format!("write {CUSTOM_CFG}: {e}"))?;
    }
    sys::freeze_thaw("/boot")?;
    info!("installed GRUB boot-counting snippet in {CUSTOM_CFG}");
    Ok(())
}

// ---- preflight and arming ------------------------------------------------

/// Run preflight on a staged deployment; returns the fatal messages.
fn check_staged(dep: &bootc::Deployment, log: &dyn Fn(&preflight::Finding)) -> Vec<String> {
    let findings = preflight::run(&dep.dir(), TOOL);
    findings.iter().for_each(log);
    findings.into_iter().filter(|f| f.fatal).map(|f| f.msg).collect()
}

fn cmd_preflight() -> Res<()> {
    let Some(staged) = bootc::status()?.staged else {
        info!("no staged deployment; nothing to check");
        return Ok(());
    };
    if !check_staged(&staged, &|f| println!("{}: {}", f.tag(), f.msg)).is_empty() {
        std::process::exit(1);
    }
    println!("preflight passed for {}", staged.digest);
    Ok(())
}

fn arm_trial(cfg: &Config, digest: &str) -> Result<(), String> {
    if !digest.chars().all(|c| c.is_ascii_alphanumeric() || ":._-".contains(c)) {
        return Err(format!("unexpected image digest {digest:?}"));
    }
    setup()?;
    let attempts = cfg.max_attempts.to_string();
    update_env(|e| {
        e.set(V_COUNTER, &attempts)?;
        e.set(V_TARGET, digest)?;
        e.set(V_FALLBACK, "1")
    })
}

/// Preflight and arm the staged deployment. An `Err` means it cannot be
/// guarded, so the caller must not let it be applied.
fn arm_staged(cfg: &Config) -> Result<(), String> {
    let st = bootc::status()?;
    let Some(staged) = st.staged else { return Ok(()) };
    if st.booted.is_none() {
        return Err("cannot determine the booted deployment".into());
    }
    if rejected_digests().contains(&staged.digest) {
        info!("staged image {} was rejected before; discarding it", staged.digest);
        return bootc::unstage();
    }
    if cfg.preflight {
        let fatals = check_staged(&staged, &|f| info!("preflight {}: {}", f.tag(), f.msg));
        if !fatals.is_empty() {
            bootc::unstage()?;
            reject(&staged.digest, &format!("preflight failed: {}", fatals.join("; ")), false);
            return Ok(());
        }
    }
    arm_trial(cfg, &staged.digest).map_err(|e| {
        let _ = clear_trial();
        format!("could not arm the boot counter for {}: {e}", staged.digest)
    })?;
    info!("armed trial for {} with {} boot attempts", staged.digest, cfg.max_attempts);
    Ok(())
}

/// Runs at shutdown, before ostree finalizes the staged deployment. Fails
/// closed: an update that cannot be guarded is not applied.
fn cmd_arm() -> Res<()> {
    let cfg = Config::load();
    if !cfg.enabled {
        info!("disabled by configuration");
        return Ok(());
    }
    if let Err(e) = arm_staged(&cfg) {
        bootc::unstage()?;
        notify(&format!("The update was not applied: {e}"));
    }
    Ok(())
}

// ---- boot assessment -----------------------------------------------------

struct Trial {
    counter: Option<i32>,
    target: Option<String>,
    status: bootc::Status,
    booted: bootc::Deployment,
}

impl Trial {
    fn load() -> Res<Trial> {
        let env = read_env()?;
        let status = bootc::status()?;
        let booted = status.booted.clone().ok_or("no booted deployment")?;
        Ok(Trial {
            counter: env.get_i32(V_COUNTER),
            target: env.get(V_TARGET).map(String::from),
            status,
            booted,
        })
    }

    fn assessment(&self) -> Assessment {
        state::assess(self.counter, self.target.as_deref(), &self.booted.digest)
    }
}

fn reboot_now() {
    if !sys::systemctl(&["reboot", "--no-block"]) {
        warn!("systemctl reboot failed; the trial watchdog will force it");
    }
}

fn ensure_default_is_booted() -> Result<(), String> {
    if bootc::booted_is_default()? {
        return Ok(());
    }
    info!("booted deployment is not the default; running bootc rollback");
    bootc::rollback()?;
    if !bootc::booted_is_default()? {
        return Err("bootc rollback ran but the booted deployment is still not the default".into());
    }
    Ok(())
}

/// Roll back from userspace (we are running the failed deployment).
fn finish_rollback(st: &bootc::Status, target: &str, reason: &str) -> Res<()> {
    if st.rollback.is_none() {
        warn!("no rollback deployment is available; abandoning the trial");
        return Ok(clear_trial()?);
    }
    bootc::rollback()?;
    reject(target, reason, true);
    clear_trial()?;
    info!("rolled back; rebooting into the previous deployment");
    reboot_now();
    Ok(())
}

fn cmd_assess() -> Res<()> {
    let t = Trial::load()?;
    match t.assessment() {
        Assessment::Idle => {}
        Assessment::Stale => {
            info!("clearing stale trial state");
            clear_trial()?;
        }
        Assessment::TrialInProgress => {
            let timeout = Config::load().trial_timeout.as_secs();
            info!("trial boot of {} (attempt counter {:?}); watchdog {timeout}s", t.booted.digest, t.counter);
            std::fs::create_dir_all(run_file(""))?;
            std::fs::write(run_file("trial"), &t.booted.digest)?;
            sys::systemctl(&["start", "--no-block", "agenticlinux-bootguard-watch.service"]);
        }
        Assessment::FellBack { target } => {
            info!("trial of {target} failed; bootloader returned us to {}", t.booted.digest);
            ensure_default_is_booted()?;
            reject(&target, "it never completed a boot (GRUB fell back to the previous deployment)", true);
            clear_trial()?;
        }
        Assessment::Exhausted { target } => {
            warn!("trial of {target} is out of boot attempts");
            return finish_rollback(&t.status, &target, "it used up all its boot attempts");
        }
    }
    Ok(())
}

// ---- health checks -------------------------------------------------------

/// Scripts in `sub` under `bases`; a later base overrides by name, even with a
/// file that is not executable, which masks the earlier one. Only executables run.
fn scripts_in(bases: &[&Path], sub: &str) -> Vec<PathBuf> {
    let mut m = std::collections::BTreeMap::new();
    for base in bases {
        for e in std::fs::read_dir(base.join(sub)).into_iter().flatten().flatten() {
            m.insert(e.file_name(), e.path());
        }
    }
    m.into_values().filter(|p| sys::is_executable(p)).collect()
}

fn collect_scripts(sub: &str) -> Vec<PathBuf> {
    scripts_in(&[Path::new("/usr/lib/agenticlinux-bootguard"), Path::new("/etc/agenticlinux-bootguard")], sub)
}

fn run_script(p: &Path, timeout: Duration) -> Result<(), String> {
    info!("running {}", p.display());
    let mut cmd = Command::new(p);
    cmd.stdin(Stdio::null());
    match sys::run_timeout(cmd, timeout) {
        Ok(Some(st)) if st.success() => Ok(()),
        Ok(Some(st)) => Err(format!("{} failed ({st})", p.display())),
        Ok(None) => Err(format!("{} timed out after {}s and was killed", p.display(), timeout.as_secs())),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn run_hooks(sub: &str, cfg: &Config) {
    for s in collect_scripts(sub) {
        let _ = run_script(&s, cfg.check_timeout);
    }
}

fn run_checks(cfg: &Config) -> Result<(), String> {
    if let Some(u) = cfg.required_units.iter().find(|u| !sys::systemctl(&["is-active", "--quiet", u])) {
        return Err(format!("required unit {u} is not active"));
    }
    if cfg.fail_on_failed_units {
        let failed = sys::capture("systemctl", &["--failed", "--no-legend", "--plain"])?;
        if let Some(l) = failed.lines().map(str::trim).find(|l| !l.is_empty()) {
            return Err(format!("failed units: {l}"));
        }
    }
    for s in collect_scripts("check/required.d") {
        run_script(&s, cfg.check_timeout)?;
    }
    for s in collect_scripts("check/wanted.d") {
        if let Err(e) = run_script(&s, cfg.check_timeout) {
            warn!("wanted check: {e}");
        }
    }
    Ok(())
}

fn confirm(cfg: &Config, digest: &str) -> Res<()> {
    // A trial left armed could later roll back a healthy deployment, so retry,
    // and without a `confirmed` marker the watchdog keeps running.
    let mut r = clear_trial();
    for _ in 0..2 {
        if r.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
        r = clear_trial();
    }
    r?;
    info!("health checks passed; confirmed {digest}");
    let _ = std::fs::remove_file(MOTD);
    let _ = std::fs::write(run_file("confirmed"), "");
    run_hooks("green.d", cfg);
    Ok(())
}

fn cmd_check() -> Res<()> {
    let t = Trial::load()?;
    if t.assessment() != Assessment::TrialInProgress {
        return Ok(());
    }
    let cfg = Config::load();
    info!("running health checks for trial deployment {}", t.booted.digest);
    let Err(why) = run_checks(&cfg) else { return confirm(&cfg, &t.booted.digest) };
    warn!("health check failed: {why}");
    run_hooks("red.d", &cfg);
    match state::on_check_failure(t.counter) {
        OnFailure::Reboot => {
            info!("attempts remain; rebooting");
            reboot_now();
            Ok(())
        }
        OnFailure::RollbackNow => finish_rollback(&t.status, &t.booted.digest, &format!("health check failed: {why}")),
    }
}

// ---- watchdog ------------------------------------------------------------

/// Reboot a trial boot that is not confirmed within TRIAL_TIMEOUT (hung boot,
/// emergency mode, hung service) so the bootloader counts the attempt. Only
/// `confirmed` stops it, so it keeps escalating if a reboot does not happen.
fn cmd_watch() -> Res<()> {
    let cfg = Config::load();
    let start = Instant::now();
    info!("trial watchdog armed for {}s", cfg.trial_timeout.as_secs());
    while start.elapsed() < cfg.trial_timeout {
        if Path::new(&run_file("confirmed")).exists() {
            info!("trial confirmed; watchdog exiting");
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    warn!("trial boot not confirmed within {}s; forcing a reboot", cfg.trial_timeout.as_secs());
    unsafe { libc::sync() };
    for (args, wait) in [(&["reboot", "--no-block"][..], 30), (&["reboot", "--force", "--no-block"], 10), (&["reboot", "--force", "--force"], 0)] {
        sys::systemctl(args);
        std::thread::sleep(Duration::from_secs(wait));
    }
    Ok(())
}

// ---- misc ----------------------------------------------------------------

fn cmd_status() -> Res<()> {
    let env = read_env()?;
    let st = bootc::status()?;
    let d = |o: &Option<bootc::Deployment>| o.as_ref().map_or("-".to_string(), |d| d.digest.clone());
    println!("booted:   {}\nstaged:   {}\nrollback: {}", d(&st.booted), d(&st.staged), d(&st.rollback));
    for k in [V_COUNTER, V_TARGET, V_FALLBACK] {
        println!("{k}: {}", env.get(k).unwrap_or("-"));
    }
    println!("grub snippet installed: {}\nrejected: {:?}", snippet_present(), rejected_digests());
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage: agenticlinux-bootguard <command>\n\
         \n  boot       (early boot) install the GRUB snippet, assess trial state\
         \n  arm        (shutdown) preflight the staged image and start a trial\
         \n  check      (after multi-user) run health checks for a trial boot\
         \n  watch      trial-boot watchdog\
         \n  preflight  run the static checks on the staged deployment\
         \n  setup      install the GRUB snippet\
         \n  status     show state\
         \n  clear      clear trial state"
    );
    std::process::exit(2)
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let r: Res<()> = match cmd.as_str() {
        "boot" => {
            let s = setup();
            if let Err(e) = &s {
                warn!("setup: {e}");
            }
            cmd_assess().and(s.map_err(Into::into))
        }
        "arm" => cmd_arm(),
        "assess" => cmd_assess(),
        "check" => cmd_check(),
        "watch" => cmd_watch(),
        "preflight" => cmd_preflight(),
        "setup" => setup().map_err(Into::into),
        "status" => cmd_status(),
        "clear" => clear_trial().map_err(Into::into),
        _ => usage(),
    };
    if let Err(e) = r {
        eprintln!("agenticlinux-bootguard: {cmd}: error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, mode: u32) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn later_base_overrides_by_name_and_can_mask() {
        let root = std::env::temp_dir().join(format!("bg-scripts-{}", std::process::id()));
        let (vendor, admin) = (root.join("usr"), root.join("etc"));
        script(&vendor.join("c"), "10-a", 0o755);
        script(&vendor.join("c"), "20-b", 0o755);
        script(&vendor.join("c"), "30-c", 0o755);
        script(&admin.join("c"), "20-b", 0o644);
        script(&admin.join("c"), "40-d", 0o755);
        script(&admin.join("c"), "50-e", 0o644);
        let got: Vec<_> = scripts_in(&[&vendor, &admin], "c").iter().map(|p| p.file_name().unwrap().to_owned()).collect();
        assert_eq!(got, ["10-a", "30-c", "40-d"]);
        std::fs::remove_dir_all(root).ok();
    }
}
