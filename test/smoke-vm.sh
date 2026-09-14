#!/bin/bash
# Runs inside the test VM as PID 1's only job (systemd.run=); output goes to
# the serial console, which test/smoke.sh reads on the host.
trap '[ $? -eq 0 ] || echo SMOKE FAIL' EXIT
set -eux
systemctl start multi-user.target network-online.target
# /boot is an idle-unmounting automount that bootc itself cannot retrigger
# (bootc-dev/bootc#2402); any other access remounts it
ls /boot >/dev/null
bootc status
docker run --rm hello-world
for c in "sbx version" "llmman --version" "opencode --version" "codex --version" \
         "claude --version" "openclaw --version" "podman run --rm quay.io/podman/hello"; do
  runuser -l test -c "$c"
done

# Agents on a local model through llmman. Skipped under pure emulation
# (arm64 CI runners have no KVM), where inference would take hours.
modprobe qemu_fw_cfg || true
accel=$(cat /sys/firmware/qemu_fw_cfg/by_name/opt/agenticlinux/accel/raw 2>/dev/null || echo unknown)
if [ "$accel" != tcg ]; then
  runuser -l test -c "llmman pull qwen3.5:0.8b"
  prompt="Reply with exactly the word OK and nothing else"
  for c in "opencode -- run '$prompt'" \
           "claude -- -p '$prompt'" \
           "codex -- exec --skip-git-repo-check '$prompt'" \
           "openclaw -- agent --local -m '$prompt'"; do
    runuser -l test -c "timeout 900 llmman launch ${c%% *} --model qwen3.5:0.8b ${c#* }"
  done
fi
echo SMOKE PASS
