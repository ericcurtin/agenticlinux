#!/bin/bash
# The smoke test. Runs either as PID 1's only job in the test VM (systemd.run=;
# output goes to the serial console, which test/smoke.sh reads on the host) or
# inside a container of the same image. The container mode runs on both
# architectures and skips what would need nested containers (docker, podman,
# llmman's container runtime); the VM mode only runs where hardware
# virtualization exists and covers everything.
finish() {
  [ "$1" -eq 0 ] && return
  # Which runtime the daemon picked and why a load failed
  tail -n 40 /home/test/.local/share/llmman/serve.log 2>/dev/null || true
  echo SMOKE FAIL
}
trap 'finish $?' EXIT
set -eux

# Every client runs with this environment, which llmman launch hands on to the
# agent and the first client to find no daemon hands on to the daemon it spawns.
#
# LLMMAN_CONTEXT_LENGTH: without it the daemon asks llama-server for the
# model's full trained context, 256k tokens for qwen3.5:0.8b, whose six
# full-attention layers need 12 KB of KV cache per token: 3.2 GB in a guest
# that has 4-8 GB for everything. The load then OOMs, llmman halves the
# context and reloads, and that loop has run until llmman gave up on the
# server ("exited before becoming ready", 24 minutes per attempt, three
# attempts, SMOKE FAIL) on a host where the guest had a little less headroom.
# 64k tokens is 800 MB and three times what the largest agent prompt needs
# (Claude Code's 20k-token system prompt plus the 4096-token output cap).
#
# A 0.8b model at default sampling now and then never emits its end token and
# generates until the context is full, hours away (seen with codex and with
# Claude Code, 14-22k tokens in and counting when the turn hit its timeout).
# Bound every agent's turn at 4096 tokens: Claude Code and OpenCode take the
# limit from these variables (both would otherwise ask for 32k and llama-server
# lets a request's max_tokens override its own --n-predict), OpenClaw's
# onboarding writes maxTokens 4096 into its config, and codex sends no
# max_tokens, so llama-server's default applies where it can be set (below).
client_env="LLMMAN_CONTEXT_LENGTH=65536 CLAUDE_CODE_MAX_OUTPUT_TOKENS=4096 OPENCODE_EXPERIMENTAL_OUTPUT_TOKEN_MAX=4096"
as_test() { runuser -l test -c "$client_env $*"; }

# llmman's --runtime auto falls through docker -> podman -> bin on any failure
# (a slow docker info, a failed image pull) and keeps that choice; under
# rootless podman the model mount gets an SELinux label llama.cpp cannot read.
# Pin it instead: the image's Docker in the VM, the binary in a container.
if systemd-detect-virt -cq; then
  # llama-server inherits the daemon's environment here (in the VM llmman runs
  # it in a container and forwards only a fixed list of variables), so codex
  # can be bounded too
  client_env="LLMMAN_RUNTIME=bin LLAMA_ARG_N_PREDICT=4096 $client_env"
  inference=1
else
  client_env="LLMMAN_RUNTIME=docker $client_env"
  # The host may skip the agent turns (smoke.sh SMOKE_INFERENCE): the nested
  # HVF guests on the Intel macOS runners are 5-10x slower than KVM or WHPX.
  modprobe qemu_fw_cfg || true
  inference=$(cat /sys/firmware/qemu_fw_cfg/by_name/opt/agenticlinux/inference/raw 2>/dev/null || echo 1)
  systemctl start multi-user.target network-online.target
  # /boot is an idle-unmounting automount that bootc itself cannot retrigger
  # (bootc-dev/bootc#2402); any other access remounts it
  ls /boot >/dev/null
  docker run --rm hello-world
  as_test "podman run --rm quay.io/podman/hello"
fi

bootc status
for c in "sbx version" "llmman --version" "herdr --version" "opencode --version" \
         "codex --version" "claude --version" "openclaw --version"; do
  as_test "$c"
done

# The desktop apps (ChatGPT, OpenCode, and Chrome where Google builds it):
# launcher, menu entry, and every shared library of the binary resolved;
# without a display that is as far as a check can go.
apps="chatgpt          chatgpt              chatgpt/ChatGPT
opencode-desktop ai.opencode.desktop  opencode-desktop/ai.opencode.desktop"
if [ "$(uname -m)" = x86_64 ]; then
  apps="$apps
google-chrome-stable google-chrome google-chrome/chrome"
else
  test -x /usr/bin/chromium-browser
fi
while read -r bin entry exe; do
  test -x "/usr/bin/$bin"
  test -e "/usr/share/applications/$entry.desktop"
  if ! libs=$(ldd "/usr/lib/$exe") || grep 'not found' <<<"$libs"; then
    echo "/usr/lib/$exe: missing or unresolved libraries"
    exit 1
  fi
done <<<"$apps"

# Agents on a local model through llmman. A turn that runs away (see above)
# fails with the token limit, in about 10 minutes at the 7-8 tokens/s these
# runners generate, and is retried; each sample is independent and a retry
# finds the prompt already in llama-server's cache, so it costs generation time
# only. Two runaways in a row have been seen. The 1800 s budget per attempt
# cannot be tightened: a legitimate Claude Code turn takes up to 25 minutes on
# the arm64 runners, nearly all of it prompt processing of its 20k-token
# system prompt.
if [ "$inference" != 0 ]; then
  # Three tries, as for the agents' turns below: the pull is a chain of
  # registry requests through the host's network and has timed out on one
  as_test "llmman pull qwen3.5:0.8b" || as_test "llmman pull qwen3.5:0.8b" ||
    as_test "llmman pull qwen3.5:0.8b"
  prompt="Reply with exactly the word OK and nothing else"
  for c in "opencode -- run '$prompt'" \
           "claude -- -p '$prompt'" \
           "codex -- exec --skip-git-repo-check '$prompt'" \
           "openclaw -- agent --local -m '$prompt'"; do
    turn="timeout 1800 llmman launch ${c%% *} --model qwen3.5:0.8b ${c#* }"
    as_test "$turn" || as_test "$turn" || as_test "$turn"
  done
fi
echo SMOKE PASS
