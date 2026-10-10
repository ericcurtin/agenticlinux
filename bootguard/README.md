# agenticlinux-bootguard

Automatic rollback for a failed `bootc upgrade --apply`.

A newly applied deployment gets a limited number of boot attempts. If it does not
come up healthy within them, the machine returns to the previous deployment by
itself and remembers not to try that image again. This covers failures that never
reach userspace (bad kernel or initramfs, kernel panic, root cannot be mounted)
and failures after it (failed or hung health checks, hung boot, emergency mode).

It replaces [greenboot](https://github.com/fedora-iot/greenboot-rs); do not
install both. See [Compared with greenboot](#compared-with-greenboot).

## Compared with greenboot

Same qemu setup and failing images for both tools (Fedora 44, greenboot 0.16.4,
bootc 1.16.13, XFS root, GRUB). "Worked around" means greenboot with its rollback
trigger fixed (`Conflicts=final.target`) and an XFS freeze after each `grubenv`
edit; on ext4 only the first is needed.

| # | Failure | greenboot as shipped | greenboot, worked around | bootguard |
|---|---|---|---|---|
| 1a | Garbage kernel | Stuck at the GRUB menu | Rolled back | Rejected before reboot, or rolled back |
| 1b | Truncated kernel | Not run | Firmware hangs | **Rejected before reboot** |
| 2 | Kernel panic | Not run | **Hangs forever** | Rolled back |
| 3 | Initramfs cannot mount root | Not run | **Stuck in the emergency shell** | Rolled back |
| 4a | Health check fails | **Reboot loop, never rolls back** (32 boots in 7 min) | Rolled back | Rolled back |
| 4b | Hang before health checks | Not run | **Hangs forever** | Rolled back |
| 4c | Emergency mode (bad fstab) | Not run | **Stuck** (root is locked) | Rolled back |
| 4d | Health check hangs | Not run | **Stays on the bad image** | Rolled back |

"Not run" rows were not tried against stock greenboot. Its rollback trigger never
runs there, so even 4a, the case it is designed for, fails.

Why greenboot misses these:

- **Its rollback trigger never runs.** The unit has no `Conflicts=final.target`, so
  `fallback=1` and the next-deployment id are never written. GRUB has nothing to
  fall back to. On ext4, after three failed checks greenboot stops with "no
  next-deployment-id set" and stays on the bad image.
- **GRUB reads a stale `grubenv` on XFS.** It cannot replay the journal, so counter
  decrements were lost (stuck at 2 for 40 boots).
- **Its counter starts only after a health check fails in userspace.** A boot that
  never reaches userspace is never counted. Adding `panic=5`, `rd.emergency=reboot`
  or a systemd job timeout to greenboot only turned hangs into endless reboot
  loops (27, 18+ and repeating boots, none rolled back).
- **No timeouts and no pre-reboot validation.** A hanging check blocks forever, and
  a bad image is only discovered by booting it.

What bootguard does about each: it arms the counter before the first boot and lets
GRUB count every attempt (2, 3, 4b, 4c); preflights the staged image (1a, 1b, 2, 3,
4c); runs a watchdog and per-check timeouts (4b, 4c, 4d); writes `grubenv` in place
and checkpoints `/boot` (XFS); and remembers rejected digests so an update timer
does not re-apply them.

## How it works

1. **Arm (shutdown, before ostree finalizes).** Preflight the staged image. If it
   is bad, discard it. Otherwise write `bg_counter=3`, `bg_target=<digest>` and
   `fallback=1` to `grubenv`.
2. **Count (bootloader).** A snippet in `/boot/grub2/custom.cfg` decrements
   `bg_counter` on every boot attempt and, at zero, boots entry 1 (the previous
   deployment). A boot that never reaches userspace is still counted.
3. **Assess (early boot).** On the trial deployment, start the watchdog. On the
   previous deployment after a fallback, make the rollback permanent
   (`bootc rollback`) and reject the image.
4. **Check (after `multi-user.target`).** Pass: clear the trial. Fail: reboot for
   another counted attempt, or roll back now on the last one.
5. **Watchdog.** A trial boot not confirmed within `TRIAL_TIMEOUT` is rebooted.

One binary, `/usr/bin/agenticlinux-bootguard`, four units:

| Unit | When | Command |
|---|---|---|
| `agenticlinux-bootguard-arm.service` | Shutdown, before `ostree-finalize-staged` | `arm` |
| `agenticlinux-bootguard-boot.service` | `sysinit.target` | `boot` |
| `agenticlinux-bootguard-check.service` | After `multi-user.target`, trial only | `check` |
| `agenticlinux-bootguard-watch.service` | Started by `boot`, trial only | `watch` |

### Preflight

`arm` refuses a staged image if any of these fail:

- **Kernel**: valid PE, EFI zboot or bzImage structure, not truncated.
- **Initramfs**: known compression after any early microcode archive, and the
  matching integrity test (`zstd`, `gzip`, `xz`, `lz4`, `bzip2`; skipped with a
  note if not installed). A plain cpio needs an `init`.
- **`root=`** in `usr/lib/bootc/kargs.d/*.toml` (parsed as TOML, honouring
  `match-architectures`) must name an existing device.
- **fstab** (staged, booted and image copies): local-device entries must exist
  unless `nofail`, `noauto` or `bind`.
- **The tool**: the image must contain an executable `agenticlinux-bootguard`.

A refused image is unstaged, recorded in `/var/lib/agenticlinux-bootguard/rejected`,
and announced in `/etc/motd.d/agenticlinux-bootguard`. If the registry still serves
it, the next `bootc upgrade` stages it again and `arm` discards it again. A new
digest is tried normally.

### Health checks

1. `REQUIRED_UNITS` must be active; with `FAIL_ON_FAILED_UNITS=true`, no unit may
   have failed (an unanswerable `systemctl` counts as failure).
2. `check/required.d/*` run in order; the first failure fails the boot.
3. `check/wanted.d/*` run; failures are only logged.
4. `green.d/*` run on success, `red.d/*` on failure.

Scripts live in `/usr/lib/agenticlinux-bootguard/` and `/etc/agenticlinux-bootguard/`
(`/etc` overrides by filename), must be executable, and are killed with their
process group after `CHECK_TIMEOUT`, so a hanging check is a failed check.

### Fail-closed behaviour

- A trial whose counter is missing or unreadable is treated as used up.
- If the boot counter cannot be written and checkpointed, the update is not applied.
- If a passing trial cannot be cleared in `grubenv` (after retries), it is not
  confirmed, so the watchdog keeps running.
- The watchdog stops only on confirmation and escalates `reboot`, `--force`,
  `--force --force`. It ignores emergency mode isolation.

### Kernel arguments

`30-bootguard.toml` adds `panic=5 rd.shell=0 rd.emergency=reboot rd.retry=30`, so a
panic or initramfs failure reboots (and is counted) instead of hanging. These are
**always on**: a genuine fault outside an upgrade reboots instead of giving an
emergency shell. Delete the file to opt out; early failures then hang.

## State

| Where | What |
|---|---|
| grubenv `bg_counter`, `bg_target`, `fallback` | Attempts left (`-1` = used up), digest under trial, GRUB entry-1 fallback on load failure |
| `/boot/grub2/custom.cfg` | GRUB snippet between `# BEGIN/END agenticlinux-bootguard`; other content kept |
| `/var/lib/agenticlinux-bootguard/` | `rejected` (digest, time, reason), `last-event.txt` |
| `/run/agenticlinux-bootguard/` | `trial` and `confirmed` markers |

`grubenv` is rewritten in place and `/boot` is then frozen and thawed, because GRUB
reads the file by block list and cannot replay an XFS/ext4 journal.

## Configuration

Defaults: `/usr/lib/agenticlinux-bootguard/bootguard.conf`. Override in
`/etc/agenticlinux-bootguard/bootguard.conf` (`KEY=VALUE`).

| Key | Default | Meaning |
|---|---|---|
| `ENABLED` | `true` | Master switch for `arm`. |
| `PREFLIGHT` | `true` | Check a staged update before rebooting into it. |
| `MAX_ATTEMPTS` | `3` | Boot attempts for the new deployment (1 to 20). |
| `TRIAL_TIMEOUT` | `600` | Seconds before the watchdog reboots (minimum 10). |
| `CHECK_TIMEOUT` | `120` | Seconds each check script may run. |
| `REQUIRED_UNITS` | empty | Units that must be active. |
| `FAIL_ON_FAILED_UNITS` | `false` | Any failed unit fails the boot. |

## Commands

```
agenticlinux-bootguard status      # deployments, trial variables, rejected digests
agenticlinux-bootguard preflight   # check the staged update; exit 1 on failure
agenticlinux-bootguard clear       # clear trial state
agenticlinux-bootguard setup       # install the GRUB snippet
agenticlinux-bootguard boot|arm|assess|check|watch   # run by systemd
```

Logs: `journalctl -t agenticlinux-bootguard`.

## Installing

Not yet wired into the image build. Build with `cargo build --release` (deps:
`libc`, `serde`, `serde_json`, `toml`), then:

```dockerfile
COPY target/release/agenticlinux-bootguard /usr/bin/agenticlinux-bootguard
COPY dist/ /
RUN systemctl enable agenticlinux-bootguard-boot.service \
                     agenticlinux-bootguard-check.service \
                     agenticlinux-bootguard-arm.service
```

Needs a bootc system on GRUB whose `grub.cfg` sources `custom.cfg` (the Fedora
default; the tool warns otherwise). Arming happens in the deployment that is
running when the update is applied, so the first update that introduces the tool
is not protected. To disable, set `ENABLED=false`, run `clear`, and remove the
marked block from `custom.cfg`.

## Testing

`cargo test` covers the `grubenv` codec, preflight checks, the trial state machine
and config parsing. Recovery was verified under qemu (aarch64, `agenticlinux:base`,
XFS root, GRUB): each failing image was a small layer on the good image, applied
with `bootc upgrade --apply` from a fresh disk. A rollback counted as permanent
only if a plain reboot stayed on the previous deployment.

| # | Failure | Result |
|---|---|---|
| 1a | Garbage kernel | Rolled back (preflight rejects; with it off, GRUB falls back) |
| 1b | Truncated kernel | Rejected by preflight; hangs the qemu firmware with it off |
| 2 | Kernel panic | Rolled back after 3 panics |
| 3 | Initramfs cannot mount root | Rolled back after 3 failures |
| 4a | Health check fails | Rolled back |
| 4b | Hang before health checks | Rolled back (watchdog and GRUB counting) |
| 4c | Emergency mode (bad fstab) | Rolled back (preflight rejects; with it off, the watchdog recovers) |
| 4d | Health check hangs | Rolled back |

A healthy upgrade is confirmed, a re-staged rejected image is discarded, and a
later good update still applies.

Other configurations, same images and checks as above:

| Configuration | Cases run | Result |
|---|---|---|
| ext4 root | Control, 1a, 2, 3, 4a, 4b, 4c, 4d, and the preflight rejections (1a, 1b, 3, 4c) | Same as XFS; every rollback permanent |
| Secure Boot | Control, 1a, 2, 3, 4a, 4b, 4c, and a kernel with a bad signature | Same as without it; every rollback permanent |

Secure Boot used edk2 stable202408 built with `SECURE_BOOT_ENABLE` and the Red Hat
and Microsoft keys enrolled (guest: `SecureBoot=1`, `SetupMode=0`, kernel lockdown
`integrity`), booting shim, GRUB and the kernel. A tampered kernel that is
structurally valid passes preflight, but shim refuses it (`bad shim signature`) and
the armed GRUB fallback rolls back to the previous deployment.

## Limitations

- GRUB only. Tested on aarch64 and the `base` variant only; x86_64 and BIOS are
  untested.
- Preflight does not verify kernel signatures. Under Secure Boot a kernel that fails
  verification is caught at its first boot by the GRUB fallback instead.
- A malformed kernel that passes preflight can hang the firmware, and a kernel that
  hangs without panicking is never caught; both need a hardware watchdog.
- A passing check proves only what it checks; add checks for your services under
  `check/required.d/`.
