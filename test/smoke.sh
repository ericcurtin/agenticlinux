#!/bin/bash
# Boot a smoke-test qcow2 (built from test/Dockerfile with bootc install) under
# qemu with hardware virtualization and wait for the in-guest smoke-vm to
# report over the serial console. Linux (KVM), macOS (HVF) and Windows Git
# Bash (WHPX); there is deliberately no emulation fallback. The disk is not
# modified: the guest writes to a throwaway overlay.
#
# Usage: test/smoke.sh disk.qcow2 [x86_64|aarch64]
# SMOKE_INFERENCE=0 has the guest skip the agent turns (see smoke-vm.sh).
set -euo pipefail

disk=$1
arch=${2:-$(uname -m)}
inference=${SMOKE_INFERENCE:-1}
mem=8G; [ "$inference" != 0 ] || mem=4G
# For the whole boot and test (the agent turns alone take 17 minutes under
# WHPX, plus up to 30 for one turn the guest times out and the two shorter,
# token-capped retries of it, see smoke-vm.sh), and for the guest's first test
# output (slowest boot seen: 4 minutes)
timeout=${SMOKE_TIMEOUT:-7200}
boot_timeout=${SMOKE_BOOT_TIMEOUT:-900}

case "$arch" in
  x86_64) qemu=qemu-system-x86_64 machine=q35 ;;
  aarch64) qemu=qemu-system-aarch64 machine=virt ;;
esac

cpu=host
smp=4
case "$(uname -s)" in
  Linux) accel=kvm; [ -w /dev/kvm ] || { echo "no usable /dev/kvm"; exit 1; } ;;
  # The macOS Intel runners have 4 cores and 14 GB, and are VMs themselves.
  # A guest with all 4 has starved the runner agent of CPU: 44 minutes into
  # one smoke test (the nine others took 10-20) GitHub ended the job with
  # "the hosted runner lost communication with the server", no log kept.
  # Leave two cores to macOS and the agent.
  Darwin) accel=hvf smp=2 ;;
  # WHPX has no "host" model, and "max" hands the guest every feature the
  # Hyper-V partition advertises, which varies with the Azure host the runner
  # lands on. On one such host the guest kernel's cred state was corrupted
  # within seconds of boot (WARN in cap_bprm_creds_from_file, every init exec
  # returned EPERM, panic) on both attempts, while the same disk booted fine
  # on other hosts. A fixed x86-64-v3 model (AVX2/FMA for llama.cpp) keeps the
  # guest's feature set, and the emulation paths it exercises, the same
  # everywhere; qemu silently drops any bit a host lacks.
  *) accel=whpx,kernel-irqchip=off cpu=Skylake-Client-noTSX-IBRS ;;
esac

# Copy the firmware next to the disk: keeps qemu's arguments free of absolute
# paths, which MSYS on Windows would otherwise rewrite.
bin=$(dirname "$(command -v "$qemu")")
for f in "$bin/../share/qemu/edk2-$arch-code.fd" "$bin/share/edk2-$arch-code.fd" \
         /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/AAVMF/AAVMF_CODE.fd; do
  case "$f" in *OVMF*) [ "$arch" = x86_64 ] || continue ;; *AAVMF*) [ "$arch" = aarch64 ] || continue ;; esac
  [ -e "$f" ] && cp "$f" firmware.fd && break
done

# A guest with no test output within the boot budget (firmware or hypervisor
# hang, seen on the macOS runners) or that panics before the test starts (seen
# under WHPX) is retried once; a failure, panic or timeout inside the test, or
# qemu itself failing (e.g. no accelerator), is not.
#
# Each attempt's console and qemu stderr are kept (serial.N.log, qemu.N.log)
# so a retried first attempt can still be read from CI's artifact.
for attempt in 1 2; do
  serial=serial.$attempt.log
  qemu_log=qemu.$attempt.log
  rm -f "$serial" "$qemu_log"
  # bootindex pins the disk as the firmware's boot target; without it EDK2 can
  # race virtio-blk enumeration, fall through to the EFI shell and hang.
  #
  # snapshot=on opens the disk read-only and sends the guest's writes to a
  # throwaway overlay: every attempt boots the same disk, and the tested image
  # is the one that is handed on. It also keeps qemu from ever writing into
  # the zstd-compressed image: on the macOS runners (and only there, the same
  # disks boot on Windows) qemu 11.1 has refused the firmware's very first
  # write into it as an "invalid write on metadata (overlaps with refcount
  # block)", set the corrupt bit in the header and so killed the retry too
  # ("Image is corrupt; cannot be opened read/write").
  #
  # 8 GB: the guest runs docker, the llmman daemon and llama-server with the
  # model and its KV cache (bounded in smoke-vm.sh). Without the agent turns
  # there is no model to hold, and 4 GB leaves the rest of the 14 GB macOS
  # Intel runner to macOS and the runner agent (see smp above).
  #
  # ipv6=off: user-mode networking gives the guest an IPv6 address and router
  # by default, but the runners have no IPv6 route out, so a registry that
  # resolves to an AAAA record first has the guest dialing it until the
  # timeout ("dial tcp [2600:...]:443: i/o timeout" from llmman pull on a
  # Windows runner) instead of failing fast and moving on to IPv4. With no
  # IPv6 on the link the guest only ever dials IPv4.
  "$qemu" -M "$machine" -accel "$accel" -cpu "$cpu" -smp "$smp" -m "$mem" -no-reboot \
    -display none -monitor none -serial "file:$serial" \
    -fw_cfg name=opt/agenticlinux/inference,string="$inference" \
    -drive if=pflash,format=raw,readonly=on,file=firmware.fd \
    -drive file="$disk",if=none,id=d0,format=qcow2,snapshot=on \
    -device virtio-blk-pci,drive=d0,bootindex=0 \
    -netdev user,id=n0,ipv6=off -device virtio-net-pci,netdev=n0 \
    -device virtio-rng-pci 2> "$qemu_log" &
  pid=$!

  # Stream the guest's own output while waiting so CI logs show progress
  tail -F "$serial" 2>/dev/null | grep --line-buffered -E "smoke-vm\[|SMOKE|BdsDxe|Kernel panic" &
  tailpid=$!
  booted=
  panicked=
  shell=
  killed=
  for ((t = 0; t < timeout; t += 10)); do
    kill -0 "$pid" 2>/dev/null || break
    [ -n "$booted" ] || ! grep -q "smoke-vm\[" "$serial" 2>/dev/null || booted=1
    # A panicked kernel sits there forever (panic= is unset, -no-reboot); don't
    # wait out the budget on it
    grep -q "Kernel panic - not syncing" "$serial" 2>/dev/null && { panicked=1; break; }
    # Nor on a firmware that gave up on the disk and dropped to its shell
    [ -n "$booted" ] || ! grep -q "Shell> " "$serial" 2>/dev/null || { shell=1; break; }
    [ -n "$booted" ] || [ "$t" -lt "$boot_timeout" ] || break
    sleep 10
  done
  kill "$pid" 2>/dev/null && killed=1
  status=0; wait "$pid" || status=$?
  kill "$tailpid" 2>/dev/null || true

  if [ -z "$killed" ] && [ "$status" -ne 0 ]; then
    cat "$qemu_log"; exit "$status"
  fi
  grep -q "SMOKE PASS" "$serial" && exit 0
  # The stream above is filtered; a panic's WARN and trace need more room
  echo "--- last lines of the serial console"
  tail -n "$([ -n "$panicked" ] && echo 100 || echo 40)" "$serial" 2>/dev/null || true
  echo "---"
  # qemu's own complaints (a refused disk write, an accelerator warning),
  # minus its note about the kill above
  if grep -qv "terminating on signal" "$qemu_log" 2>/dev/null; then
    echo "--- qemu"; grep -v "terminating on signal" "$qemu_log"; echo "---"
  fi
  grep -q "SMOKE FAIL" "$serial" && exit 1
  if [ -n "$booted" ]; then
    if [ -n "$panicked" ]; then echo "the guest kernel panicked during the test"
    else echo "no result from the guest after ${timeout}s"; fi
    exit 1
  fi
  if [ -n "$panicked" ]; then echo "attempt $attempt: the guest kernel panicked before the test ran"
  elif [ -n "$shell" ]; then echo "attempt $attempt: the firmware fell through to the EFI shell"
  else echo "attempt $attempt: no output from the guest after ${boot_timeout}s"; fi
done
exit 1
