# agenticlinux-bootguard

Automatic rollback for a failed `bootc upgrade --apply`.

A newly applied deployment gets a limited number of boot attempts. If it does not
come up healthy within them, the machine returns to the previous deployment by
itself and remembers not to try that image again. This covers failures that never
reach userspace (bad kernel or initramfs, kernel panic, root cannot be mounted)
and failures after it (failed or hung health checks, hung boot, emergency mode).

It supports two boot stacks: ostree with GRUB, and bootc's composefs backend with
systemd-boot (see [Boot stacks](#boot-stacks)). It replaces
[greenboot](https://github.com/fedora-iot/greenboot-rs); do not install both.
See [Compared with greenboot](#compared-with-greenboot).

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

## Boot stacks

The flow below is the same on both; only the boot counter differs. The stack is
detected from `bootc status` at run time.

| | ostree + GRUB | composefs + systemd-boot |
|---|---|---|
| Counter | `bg_counter` in `grubenv`, decremented by a snippet in `custom.cfg` | The boot entry's name: `x+3.conf`, renamed `x+2-1.conf` by systemd-boot on each attempt |
| Arm | Write the counter before ostree finalizes | Rename the staged entry in `/boot/loader/entries.staged` before bootc finalizes |
| Exhausted | GRUB boots entry 1 | The entry is bad (`+0-3`) and sorts last, so the previous deployment boots |
| Confirm | Clear the variables | Strip the counter from the entry |
| Make rollback permanent | `bootc rollback` | Already permanent: the bad entry stays demoted |
| Discard a staged update | Remove `/run/ostree/staged-deployment` | Remove `/run/composefs/staged-deployment` and `entries.staged` |
| Preflight reads | The staged ostree deployment | The staged image, mounted read-only with `mount.composefs` |

systemd-boot is only supported by bootc's composefs backend; bootc refuses it on
the ostree backend (`bootupd is required for ostree-based installs`), and ostree
keeps its kernels on a filesystem systemd-boot cannot read. A composefs image needs
`systemd-boot-unsigned` and an initramfs built with bootc's dracut module
(`dracut --omit ostree --add bootc`).

`systemd-bless-boot.service` is masked (`dist/etc/systemd/system`): it would bless a
boot without our health checks, and Fedora's SELinux policy denies its rename on
the FAT ESP anyway. bootguard does the rename itself, from `/usr/bin`, which runs
unconfined.

## How it works

1. **Arm (shutdown, before the update is finalized).** Preflight the staged image.
   If it is bad, discard it. Otherwise start a trial with 3 boot attempts.
2. **Count (bootloader).** GRUB or systemd-boot decrements the counter on every
   boot attempt and, at zero, boots the previous deployment. A boot that never
   reaches userspace is still counted.
3. **Assess (early boot).** On the trial deployment, start the watchdog. On the
   previous deployment after a fallback, make the rollback permanent and reject
   the image.
4. **Check (after `multi-user.target`).** Pass: confirm the trial. Fail: reboot for
   another counted attempt, or roll back now on the last one.
5. **Watchdog.** A trial boot not confirmed within `TRIAL_TIMEOUT` is rebooted.

One binary, `/usr/bin/agenticlinux-bootguard`, four units:

| Unit | When | Command |
|---|---|---|
| `agenticlinux-bootguard-arm.service` | Shutdown, before `ostree-finalize-staged` or `bootc-finalize-staged` | `arm` |
| `agenticlinux-bootguard-boot.service` | `sysinit.target` | `boot` |
| `agenticlinux-bootguard-check.service` | After `multi-user.target`, trial only | `check` |
| `agenticlinux-bootguard-watch.service` | Started by `boot`, trial only | `watch` |

### Preflight

`arm` refuses a staged image if any of these fail:

- **Kernel**: valid PE32+, EFI zboot or bzImage structure, not truncated (a raw
  arm64 `Image` only needs its header).
- **Initramfs**: known compression after any early microcode archive, and the
  matching integrity test (`zstd`, `gzip`, `xz`, `lz4`, `bzip2`; skipped with a
  note if not installed). A plain cpio needs an executable `init` or a symlink
  (its target is not followed), and its `070702` checksums must match.
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
(`/etc` overrides by filename; a non-executable override masks the vendor script),
must be executable, and are killed with their process group after `CHECK_TIMEOUT`,
so a hanging check is a failed check.

### Fail-closed behaviour

- A trial whose counter is missing or unreadable is treated as used up.
- If the boot counter cannot be written and checkpointed, `grub.cfg` does not source
  `custom.cfg`, or `bootc status` fails or shows no booted deployment, the update
  is not applied.
- A required check directory that cannot be read fails the boot. So does an
  unreadable `kargs.d` or fstab in preflight.
- Commands whose output we read (`bootc status`, ...) time out after 30 s, and ones
  we only check for success (`bootc rollback`, mounts, `systemctl`) after 60 s, so a
  hung `bootc` cannot stall the boot service that starts the trial's watchdog.
- If a trial boot cannot be assessed (for example the new image's `bootc` is broken),
  it is still guarded: the bootloader's own counter shows a trial is armed, so the
  watchdog starts and each attempt is counted until the bootloader falls back.
- Rolling back is idempotent: if a half-finished rollback left a stale counter, the
  next boot does not swap the failed deployment back to default.
- If a passing trial cannot be cleared in the bootloader state (after retries), it is
  not confirmed, so the watchdog keeps running.
- The watchdog stops only on confirmation and escalates `reboot`, `--force`,
  `--force --force`, then a direct `reboot(2)`. It ignores emergency mode isolation.
  If it cannot be started, the trial is rebooted so GRUB counts the attempt.

### Kernel arguments

`30-bootguard.toml` adds `panic=5 rd.shell=0 rd.emergency=reboot rd.retry=30`, so a
panic or initramfs failure reboots (and is counted) instead of hanging. These are
**always on**: a genuine fault outside an upgrade reboots instead of giving an
emergency shell. Delete the file to opt out; early failures then hang.

## State

| Where | What |
|---|---|
| grubenv `bg_counter`, `bg_target`, `fallback` | Attempts left (`-1` = used up), digest under trial, GRUB entry-1 fallback on load failure (bootguard owns `fallback` during a trial, as greenboot does) |
| `/boot/grub2/custom.cfg` | GRUB snippet between `# BEGIN/END agenticlinux-bootguard`; other content kept |
| `/boot/loader/entries/*+N-M.conf` | systemd-boot: the trial's boot entry; N tries left, M done |
| `/var/lib/agenticlinux-bootguard/` | `rejected` (digest, time, reason), `last-event.txt`, `trial` (systemd-boot: image digest and composefs id under trial) |
| `/run/agenticlinux-bootguard/` | `trial` and `confirmed` markers |

`grubenv` is rewritten in place and `/boot` is then frozen and thawed, because GRUB
reads the file by block list and cannot replay an XFS/ext4 journal. The systemd-boot
ESP is FAT, which has no journal, so a `sync` after the rename is enough.

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
agenticlinux-bootguard clear       # clear trial state and stop the watchdog
agenticlinux-bootguard setup       # install the GRUB snippet
agenticlinux-bootguard boot|arm|assess|check|watch   # run by systemd
```

Logs: `journalctl -t agenticlinux-bootguard`.

## Installing

agenticlinux's `Dockerfile` does this: a stage on each variant's own base (so the
binary links against the distribution it runs on) runs `cargo test --locked` and
`cargo build --release --locked`, and the image gets the binary, `dist/` and the
three enabled units. For another image, build with `cargo build --release --locked`
(deps: `libc`, `serde`, `serde_json`, `toml`), then:

```dockerfile
COPY target/release/agenticlinux-bootguard /usr/bin/agenticlinux-bootguard
COPY dist/ /
RUN systemctl enable agenticlinux-bootguard-boot.service \
                     agenticlinux-bootguard-check.service \
                     agenticlinux-bootguard-arm.service
```

Needs either an ostree system on GRUB whose `grub.cfg` sources `custom.cfg` (the
Fedora default), or a composefs system on a systemd-boot that supports boot
counting; otherwise `arm` refuses to apply updates. Arming happens in the
deployment that is running when the update is applied, so the first update that
introduces the tool is not protected. To disable, set `ENABLED=false`, run `clear`, and remove the
marked block from `custom.cfg`.

## Testing

`cargo test` covers the `grubenv` codec, preflight, the trial state machine,
script discovery and config parsing.

Recovery was verified under qemu on aarch64 with HVF (no emulation), on
`agenticlinux:base` with an XFS root and GRUB, and again on a composefs root with
systemd-boot (below).
Each failing image is a small layer on the good image, applied with `bootc upgrade
--apply` from a fresh disk; the results are the bootguard column above. A rollback
counted as permanent only if a plain reboot stayed on the previous deployment.
Cases that preflight rejects were also run with `PREFLIGHT=false` to exercise the
runtime path: 1a falls back in GRUB, 2 and 3 roll back after three failed boots,
4c is recovered by the watchdog, and 1b hangs the qemu firmware. A healthy upgrade
is confirmed, a re-staged rejected image is discarded, and a later good update
still applies. An image with `bootc` removed (so its trial cannot be assessed) falls
back after three counted attempts on both stacks.

| Configuration | Cases run | Result |
|---|---|---|
| ext4 root | Control, 1a, 2, 3, 4a, 4b, 4c, 4d, preflight rejections | Same as XFS |
| Secure Boot | Control, 1a, 2, 3, 4a, 4b, 4c, a kernel with a bad signature | Same as without it |
| composefs + systemd-boot (ext4 root) | Control, 1a, 1b, 2, 3, 4a, 4b, 4c, 4d; 1a, 2, 3, 4c again with `PREFLIGHT=false` | Same results; preflight rejects 1a, 1b, 2, 3 and 4c before any reboot; the rest roll back on systemd-boot's own counter |

Secure Boot used edk2 stable202408 built with `SECURE_BOOT_ENABLE` and the Red Hat
and Microsoft keys enrolled (guest: `SecureBoot=1`, `SetupMode=0`, lockdown
`integrity`). A tampered kernel that is structurally valid passes preflight, but
shim refuses it (`bad shim signature`) and the armed GRUB fallback rolls back.

On systemd-boot a healthy upgrade is also confirmed (the counter is stripped), a
re-staged rejected image is discarded at shutdown, and a later good update applies.

## Limitations

- Only ostree + GRUB and composefs + systemd-boot. Tested on aarch64 and the `base`
  variant only; x86_64 and BIOS are untested.
- bootc's composefs backend is experimental, and moving to it needs image changes
  (see [Boot stacks](#boot-stacks)). On systemd-boot, 1b (truncated kernel) was only
  run with preflight on, and Secure Boot is unsupported: Fedora ships only an
  unsigned systemd-boot.
- Preflight does not verify kernel signatures. Under Secure Boot a kernel that fails
  verification is caught at its first boot by the bootloader fallback instead.
- When a failed health check rolls a trial back, the notice is written to the failed
  deployment's `/etc`, so it is not shown after the reboot; it is still recorded in
  `/var/lib/agenticlinux-bootguard/last-event.txt`.
- A malformed kernel that passes preflight can hang the firmware, and a kernel that
  hangs without panicking is never caught; both need a hardware watchdog.
- Preflight tests that a compressed initramfs is intact, not what is inside it.
- A passing check proves only what it checks; add checks for your services under
  `check/required.d/`.
