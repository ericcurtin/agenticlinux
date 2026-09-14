#!/bin/bash
# Boot a smoke-test qcow2 (built from test/Dockerfile with bootc install) under
# qemu with hardware virtualization and wait for the in-guest smoke-vm to
# report over the serial console. Linux (KVM), macOS (HVF) and Windows Git
# Bash (WHPX); there is deliberately no emulation fallback.
#
# Usage: test/smoke.sh disk.qcow2 [x86_64|aarch64]
# SMOKE_INFERENCE=0 has the guest skip the agent turns (see smoke-vm.sh).
set -euo pipefail

disk=$1
arch=${2:-$(uname -m)}
inference=${SMOKE_INFERENCE:-1}
# For the whole boot and test (the agent turns alone take 17 minutes under
# WHPX), and for the guest's first test output (slowest boot seen: 4 minutes)
timeout=${SMOKE_TIMEOUT:-3600}
boot_timeout=${SMOKE_BOOT_TIMEOUT:-900}

case "$arch" in
  x86_64) qemu=qemu-system-x86_64 machine=q35 ;;
  aarch64) qemu=qemu-system-aarch64 machine=virt ;;
esac

cpu=host
case "$(uname -s)" in
  Linux) accel=kvm; [ -w /dev/kvm ] || { echo "no usable /dev/kvm"; exit 1; } ;;
  Darwin) accel=hvf ;;
  *) accel=whpx,kernel-irqchip=off cpu=max ;;
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
# hang, seen on the macOS runners) is retried once; a failure or timeout inside
# the guest, or qemu itself failing (e.g. no accelerator), is not.
for attempt in 1 2; do
  rm -f serial.log
  # bootindex pins the disk as the firmware's boot target; without it EDK2 can
  # race virtio-blk enumeration, fall through to the EFI shell and hang.
  "$qemu" -M "$machine" -accel "$accel" -cpu "$cpu" -smp 4 -m 4G -no-reboot \
    -display none -monitor none -serial file:serial.log \
    -fw_cfg name=opt/agenticlinux/inference,string="$inference" \
    -drive if=pflash,format=raw,readonly=on,file=firmware.fd \
    -drive file="$disk",if=none,id=d0,format=qcow2 \
    -device virtio-blk-pci,drive=d0,bootindex=0 \
    -netdev user,id=n0 -device virtio-net-pci,netdev=n0 \
    -device virtio-rng-pci 2> qemu.log &
  pid=$!

  # Stream the guest's own output while waiting so CI logs show progress
  tail -F serial.log 2>/dev/null | grep --line-buffered -E "smoke-vm\[|SMOKE|BdsDxe" &
  tailpid=$!
  booted=
  killed=
  for ((t = 0; t < timeout; t += 10)); do
    kill -0 "$pid" 2>/dev/null || break
    if [ -z "$booted" ]; then
      grep -q "smoke-vm\[" serial.log 2>/dev/null && booted=1
      [ -n "$booted" ] || [ "$t" -lt "$boot_timeout" ] || break
    fi
    sleep 10
  done
  kill "$pid" 2>/dev/null && killed=1
  status=0; wait "$pid" || status=$?
  kill "$tailpid" 2>/dev/null || true

  if [ -z "$killed" ] && [ "$status" -ne 0 ]; then
    cat qemu.log; exit "$status"
  fi
  grep -q "SMOKE PASS" serial.log && exit 0
  # The stream above is filtered
  echo "--- last lines of the serial console"
  tail -n 40 serial.log 2>/dev/null || true
  echo "---"
  grep -q "SMOKE FAIL" serial.log && exit 1
  if [ -n "$booted" ]; then
    echo "no result from the guest after ${timeout}s"; exit 1
  fi
  echo "attempt $attempt: no output from the guest after ${boot_timeout}s"
done
exit 1
