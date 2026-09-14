#!/bin/bash
# The smoke test. Runs either as PID 1's only job in the test VM (systemd.run=;
# output goes to the serial console, which test/smoke.sh reads on the host) or
# inside a container of the same image. The container mode runs on both
# architectures and skips what would need nested containers (docker, podman,
# llmman's container runtime); the VM mode only runs where hardware
# virtualization exists and covers everything.
trap '[ $? -eq 0 ] || echo SMOKE FAIL' EXIT
set -eux

if systemd-detect-virt -cq; then
  container=1
  # llama.cpp as a downloaded binary instead of llmman's default container
  runuser -l test -c "llmman serve --runtime bin" &> /var/log/llmman-serve.log &
else
  systemctl start multi-user.target network-online.target
  # /boot is an idle-unmounting automount that bootc itself cannot retrigger
  # (bootc-dev/bootc#2402); any other access remounts it
  ls /boot >/dev/null
  docker run --rm hello-world
  runuser -l test -c "podman run --rm quay.io/podman/hello"
fi

bootc status
for c in "sbx version" "llmman --version" "opencode --version" "codex --version" \
         "claude --version" "openclaw --version"; do
  runuser -l test -c "$c"
done

# Agents on a local model through llmman
runuser -l test -c "llmman pull qwen3.5:0.8b"
prompt="Reply with exactly the word OK and nothing else"
for c in "opencode -- run '$prompt'" \
         "claude -- -p '$prompt'" \
         "codex -- exec --skip-git-repo-check '$prompt'" \
         "openclaw -- agent --local -m '$prompt'"; do
  runuser -l test -c "timeout 900 llmman launch ${c%% *} --model qwen3.5:0.8b ${c#* }"
done
echo SMOKE PASS
