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
echo SMOKE PASS
