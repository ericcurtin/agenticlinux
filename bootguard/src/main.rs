//! Automatic rollback for failed `bootc upgrade --apply`. See README.md.

mod backend;
mod bootc;
mod config;
mod grub;
mod grubenv;
mod preflight;
mod sdboot;
mod state;
mod sys;

use backend::Backend;
use config::Config;
use state::{Assessment, OnFailure};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sys::Res;

const TOOL: &str = "/usr/bin/agenticlinux-bootguard";
const MOTD: &str = "/etc/motd.d/agenticlinux-bootguard";
const WATCHDOG: &str = "agenticlinux-bootguard-watch.service";

fn run_file(name: &str) -> String {
    format!("/run/agenticlinux-bootguard/{name}")
}
fn state_file(name: &str) -> String {
    format!("/var/lib/agenticlinux-bootguard/{name}")
}

// ---- records and notices -------------------------------------------------

fn record(file: &str, line: &str) {
    use std::io::Write;
    let path = state_file(file);
    let r = std::fs::create_dir_all(state_file(""))
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path))
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = r {
        warn!("cannot write {path}: {e}"); // a lost record only means a re-trial
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

// ---- preflight and arming ------------------------------------------------

/// Run preflight on a staged deployment; returns the fatal messages.
fn check_staged(b: Backend, dep: &bootc::Deployment, log: &dyn Fn(&preflight::Finding)) -> Result<Vec<String>, String> {
    let findings = b.with_staged_tree(dep, |tree, etc| preflight::run(tree, TOOL, etc))?;
    findings.iter().for_each(log);
    Ok(findings.into_iter().filter(|f| f.fatal).map(|f| f.msg).collect())
}

fn cmd_preflight() -> Res<()> {
    let st = bootc::status()?;
    let Some(staged) = st.staged.clone() else {
        info!("no staged deployment; nothing to check");
        return Ok(());
    };
    if !check_staged(Backend::detect(&st)?, &staged, &|f| println!("{}: {}", f.tag(), f.msg))?.is_empty() {
        std::process::exit(1);
    }
    println!("preflight passed for {}", staged.digest);
    Ok(())
}

/// Preflight and arm the staged deployment. An `Err` means it cannot be
/// guarded, so the caller must not let it be applied.
fn arm_staged(cfg: &Config) -> Result<(), String> {
    let st = bootc::status()?;
    let b = Backend::detect(&st)?;
    let Some(staged) = st.staged else { return Ok(()) };
    if rejected_digests().contains(&staged.digest) {
        info!("staged image {} was rejected before; discarding it", staged.digest);
        return b.unstage().map(|_| ());
    }
    if cfg.preflight {
        let fatals = check_staged(b, &staged, &|f| info!("preflight {}: {}", f.tag(), f.msg))?;
        if !fatals.is_empty() {
            b.unstage()?;
            reject(&staged.digest, &format!("preflight failed: {}", fatals.join("; ")), false);
            return Ok(());
        }
    }
    b.arm(&staged, cfg.max_attempts).map_err(|e| {
        b.discard_arm();
        format!("could not arm the boot counter for {}: {e}", staged.digest)
    })?;
    info!("armed trial for {} with {} boot attempts", staged.digest, cfg.max_attempts);
    Ok(())
}

/// Runs at shutdown, before the staged deployment is finalized. Fails closed:
/// an update that cannot be guarded is not applied.
fn cmd_arm() -> Res<()> {
    let cfg = Config::load();
    if !cfg.enabled {
        info!("disabled by configuration");
        return Ok(());
    }
    if let Err(e) = arm_staged(&cfg) {
        // The stack may be the very thing that failed to detect: drop either.
        if backend::unstage_any()? {
            notify(&format!("The update was not applied: {e}"));
        } else {
            warn!("{e}");
        }
    }
    Ok(())
}

// ---- boot assessment -----------------------------------------------------

struct Trial {
    backend: Backend,
    counter: Option<i32>,
    target: Option<String>,
    status: bootc::Status,
    booted: bootc::Deployment,
}

impl Trial {
    fn load() -> Res<Trial> {
        let status = bootc::status()?;
        let backend = Backend::detect(&status)?;
        let booted = status.booted.clone().ok_or("no booted deployment")?;
        let (counter, target) = backend.trial(&booted)?;
        Ok(Trial { backend, counter, target, status, booted })
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

/// Reboot ever harder, ending in reboot(2) in case systemctl is unusable.
fn force_reboot() {
    unsafe { libc::sync() };
    for (args, wait) in [(&["reboot", "--no-block"][..], 30), (&["reboot", "--force", "--no-block"], 10), (&["reboot", "--force", "--force"], 0)] {
        if sys::systemctl(args) {
            std::thread::sleep(Duration::from_secs(wait));
        } else {
            warn!("systemctl {} failed", args.join(" "));
        }
    }
    unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_RESTART) };
}

/// Roll back from userspace (we are running the failed deployment).
fn finish_rollback(t: &Trial, target: &str, reason: &str) -> Res<()> {
    if t.status.rollback.is_none() {
        warn!("no rollback deployment is available; abandoning the trial");
        return Ok(t.backend.confirm()?);
    }
    t.backend.roll_back_from(&t.booted)?;
    reject(target, reason, true);
    sys::retry(|| t.backend.forget())?;
    info!("rolled back; rebooting into the previous deployment");
    force_reboot(); // the Exhausted path has no watchdog to fall back on
    Ok(())
}

/// Mark a trial boot and start its watchdog. An unguarded trial must not run:
/// if the watchdog will not start, reboot so the bootloader counts the attempt.
fn guard_trial(digest: &str) -> Res<()> {
    std::fs::create_dir_all(run_file(""))?;
    std::fs::write(run_file("trial"), digest)?;
    if !(sys::systemctl(&["start", WATCHDOG]) && sys::systemctl(&["is-active", "--quiet", WATCHDOG])) {
        warn!("cannot start the trial watchdog; rebooting so the attempt counts");
        force_reboot();
    }
    Ok(())
}

fn cmd_assess() -> Res<()> {
    let t = Trial::load()?;
    match t.assessment() {
        Assessment::Idle => {}
        Assessment::Stale => {
            info!("clearing stale trial state");
            t.backend.confirm()?;
        }
        Assessment::TrialInProgress => {
            let timeout = Config::load().trial_timeout.as_secs();
            info!("trial boot of {} (attempt counter {:?}); watchdog {timeout}s", t.booted.digest, t.counter);
            guard_trial(&t.booted.digest)?;
        }
        Assessment::FellBack { target } => {
            info!("trial of {target} failed; bootloader returned us to {}", t.booted.digest);
            t.backend.make_fallback_permanent()?;
            reject(&target, "it never completed a boot (the bootloader fell back to the previous deployment)", true);
            sys::retry(|| t.backend.forget())?;
        }
        Assessment::Exhausted { target } => {
            warn!("trial of {target} is out of boot attempts");
            return finish_rollback(&t, &target, "it used up all its boot attempts");
        }
    }
    Ok(())
}

// ---- health checks -------------------------------------------------------

/// Executable scripts in `sub`; a later base overrides by name (a non-executable
/// file masks). A missing directory is empty; other read errors are returned.
fn scripts_in(bases: &[&Path], sub: &str) -> Result<Vec<PathBuf>, String> {
    let mut m = std::collections::BTreeMap::new();
    for dir in bases.iter().map(|b| b.join(sub)) {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot read {}: {e}", dir.display())),
        };
        for e in rd {
            let e = e.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
            m.insert(e.file_name(), e.path());
        }
    }
    Ok(m.into_values().filter(|p| sys::is_executable(p)).collect())
}

fn collect_scripts(sub: &str) -> Result<Vec<PathBuf>, String> {
    scripts_in(&[Path::new("/usr/lib/agenticlinux-bootguard"), Path::new("/etc/agenticlinux-bootguard")], sub)
}

/// Like `collect_scripts`, for scripts that are not required: log and skip.
fn optional_scripts(sub: &str) -> Vec<PathBuf> {
    collect_scripts(sub).unwrap_or_else(|e| {
        warn!("{e}");
        vec![]
    })
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
    for s in optional_scripts(sub) {
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
    for s in collect_scripts("check/required.d")? {
        run_script(&s, cfg.check_timeout)?;
    }
    for s in optional_scripts("check/wanted.d") {
        if let Err(e) = run_script(&s, cfg.check_timeout) {
            warn!("wanted check: {e}");
        }
    }
    Ok(())
}

/// End the trial's watchdog: the `confirmed` marker, and stopping its unit as a
/// backstop. Either is enough; only if both fail could it reboot a healthy boot.
fn end_watchdog() -> Res<()> {
    let marked = std::fs::create_dir_all(run_file("")).and_then(|()| std::fs::write(run_file("confirmed"), ""));
    let stopped = sys::systemctl(&["stop", WATCHDOG]);
    match marked {
        Err(e) if !stopped => Err(format!("cannot end the trial watchdog: {e}").into()),
        _ => Ok(()),
    }
}

fn confirm(t: &Trial, cfg: &Config) -> Res<()> {
    // Retry: a trial left armed could later roll back a healthy deployment.
    sys::retry(|| t.backend.confirm())?;
    info!("health checks passed; confirmed {}", t.booted.digest);
    let _ = std::fs::remove_file(MOTD);
    end_watchdog()?;
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
    let Err(why) = run_checks(&cfg) else { return confirm(&t, &cfg) };
    warn!("health check failed: {why}");
    if Trial::load()?.assessment() != Assessment::TrialInProgress {
        info!("trial was cleared during the checks; not acting on the failure");
        return Ok(());
    }
    run_hooks("red.d", &cfg);
    match state::on_check_failure(t.counter) {
        OnFailure::Reboot => {
            info!("attempts remain; rebooting");
            reboot_now();
            Ok(())
        }
        OnFailure::RollbackNow => finish_rollback(&t, &t.booted.digest, &format!("health check failed: {why}")),
    }
}

// ---- watchdog ------------------------------------------------------------

/// Reboot a trial boot not confirmed within TRIAL_TIMEOUT so the bootloader
/// counts the attempt. Only `confirmed` stops it, so it keeps escalating.
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
    loop {
        force_reboot();
        std::thread::sleep(Duration::from_secs(10));
    }
}

// ---- misc ----------------------------------------------------------------

fn backend() -> Result<Backend, String> {
    Backend::detect(&bootc::status()?)
}

fn cmd_status() -> Res<()> {
    let st = bootc::status()?;
    let b = Backend::detect(&st)?;
    let d = |o: &Option<bootc::Deployment>| o.as_ref().map_or("-".to_string(), |d| d.digest.clone());
    println!("booted:   {}\nstaged:   {}\nrollback: {}", d(&st.booted), d(&st.staged), d(&st.rollback));
    println!("backend: {}", if b == Backend::Grub { "ostree + GRUB" } else { "composefs + systemd-boot" });
    b.report()?.iter().for_each(|l| println!("{l}"));
    println!("rejected: {:?}", rejected_digests());
    Ok(())
}

/// Boot service: check boot counting works, then assess the trial either way.
fn cmd_boot() -> Res<()> {
    let setup = backend().and_then(Backend::setup);
    if let Err(e) = &setup {
        warn!("setup: {e}");
    }
    let assessed = cmd_assess();
    if let Err(e) = &assessed {
        // Without bootc status we cannot judge a trial, but a hang must still
        // be rebooted and counted: guard any trial the bootloader says is armed.
        warn!("cannot assess the boot: {e}");
        if backend::trial_armed_without_bootc() {
            guard_trial("unknown")?;
        }
    }
    assessed.and(setup.map_err(Into::into))
}

fn cmd_clear() -> Res<()> {
    backend()?.confirm()?;
    end_watchdog()
}

fn usage() -> ! {
    eprintln!(
        "usage: agenticlinux-bootguard <command>\n\
         \n  boot       (early boot) set up boot counting, assess trial state\
         \n  arm        (shutdown) preflight the staged image and start a trial\
         \n  check      (after multi-user) run health checks for a trial boot\
         \n  watch      trial-boot watchdog\
         \n  preflight  run the static checks on the staged deployment\
         \n  setup      set up boot counting (installs the GRUB snippet)\
         \n  status     show state\
         \n  clear      clear trial state and stop the watchdog"
    );
    std::process::exit(2)
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let r: Res<()> = match cmd.as_str() {
        "boot" => cmd_boot(),
        "arm" => cmd_arm(),
        "assess" => cmd_assess(),
        "check" => cmd_check(),
        "watch" => cmd_watch(),
        "preflight" => cmd_preflight(),
        "setup" => backend().and_then(Backend::setup).map_err(Into::into),
        "status" => cmd_status(),
        "clear" => cmd_clear(),
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
        let got: Vec<_> = scripts_in(&[&vendor, &admin], "c").unwrap().iter().map(|p| p.file_name().unwrap().to_owned()).collect();
        assert_eq!(got, ["10-a", "30-c", "40-d"]);
        assert!(scripts_in(&[&vendor], "missing").unwrap().is_empty());
        std::fs::write(vendor.join("file"), "").unwrap();
        assert!(scripts_in(&[&vendor], "file").is_err(), "not a directory is an error");
        std::fs::remove_dir_all(root).ok();
    }
}
