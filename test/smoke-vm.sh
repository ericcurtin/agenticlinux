#!/bin/bash
# The smoke test. Runs either as PID 1's only job in the test VM (systemd.run=;
# output goes to the serial console, which test/smoke.sh reads on the host) or
# inside a container of the same image. The container mode runs on both
# architectures and skips what would need nested containers (docker, podman,
# llmman's container runtime); the VM mode only runs where hardware
# virtualization exists and covers everything.
llmman_data=/home/test/.local/share/llmman
finish() {
  [ "$1" -eq 0 ] && return
  # Which runtime the daemon picked and why a load failed
  tail -n 40 "$llmman_data/serve.log" 2>/dev/null || true
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

# Run a command up to three times. Clients crash now and then in the macOS
# runners' guests (CodexBar segfaulted, goose aborted after its version); each
# failed try is logged with the kernel's crash lines, so a retry stays visible.
retry() {
  local i status=0
  for i in 1 2 3; do
    "$@" && return 0
    status=$?
    echo "attempt $i of 3 failed with status $status: $*"
    dmesg 2>/dev/null | grep -E 'segfault|general protection|traps:' | tail -n 3 || true
  done
  return "$status"
}

# llmman's --runtime auto falls through docker -> podman -> bin on any failure
# (a slow docker info, a failed image pull) and keeps that choice; under
# rootless podman the model mount gets an SELinux label llama.cpp cannot read.
# Pin it instead: the image's Docker in the VM, the binary in a container.
if systemd-detect-virt -cq; then
  # llama-server inherits the daemon's environment here (in the VM llmman runs
  # it in a container and forwards only a fixed list of variables), so codex
  # can be bounded too. LLMMAN_LLM_LIBRARY=cpu is what a GPU-less container
  # gets anyway; it fixes the backend that seed_llama_cpp has to fill.
  client_env="LLMMAN_RUNTIME=bin LLMMAN_LLM_LIBRARY=cpu LLAMA_ARG_N_PREDICT=4096 $client_env"
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
  # Rootless Docker as a user sets it up. The tool needs the user's systemd,
  # which lingering starts (asynchronously, hence the start); --force because
  # test can reach the rootful socket; no pager, which on an unknown TERM
  # fails the tool's systemd check. hello-world comes from the rootful daemon,
  # not a second pull. Uninstalling frees the memory for the agent turns and
  # points the CLI back at the rootful daemon.
  uid=$(id -u test)
  loginctl enable-linger test
  systemctl start "user@$uid.service"
  runuser -l test -c "export XDG_RUNTIME_DIR=/run/user/$uid SYSTEMD_PAGER=
    dockerd-rootless-setuptool.sh install --force &&
    docker --context default save hello-world | docker --context rootless load -q &&
    docker --context rootless run --rm --pull never hello-world &&
    dockerd-rootless-setuptool.sh uninstall --force"
  loginctl disable-linger test
fi

# /tmp is /var/tmp bind-mounted over it, not a tmpfs (tmp.mount.d in this
# image): the same directory, on the root filesystem, nosuid and nodev. A
# container has no tmp.mount.
if ! systemd-detect-virt -cq; then
  test "$(stat -c %d:%i /tmp)" = "$(stat -c %d:%i /var/tmp)"
  test "$(findmnt -no FSTYPE /tmp)" != tmpfs
  findmnt -no OPTIONS /tmp | grep -q nosuid
  findmnt -no OPTIONS /tmp | grep -q nodev
fi

bootc status
for c in "sbx version" "llmman --version" "herdr --version" "opencode --version" \
         "codex --version" "claude --version" "openclaw --version" "goose --version" \
         "codexbar --version" "qemu-img --version"; do
  retry as_test "$c"
done

# The desktop apps: launcher, menu entry, and every shared library of the
# binary resolved; without a display that is as far as a check can go.
while read -r bin entry exe; do
  test -x "/usr/bin/$bin"
  test -e "/usr/share/applications/$entry.desktop"
  if ! libs=$(ldd "/usr/$exe") || grep 'not found' <<<"$libs"; then
    echo "/usr/$exe: missing or unresolved libraries"
    exit 1
  fi
done <<EOF
chatgpt              chatgpt               lib/chatgpt/ChatGPT
opencode-desktop     ai.opencode.desktop   lib/opencode-desktop/ai.opencode.desktop
goose-desktop        Goose                 lib/Goose/Goose
google-chrome-stable google-chrome         lib/google-chrome/chrome
codexbar-linux       com.steipete.CodexBar bin/codexbar-linux
EOF
# Except CodexBar's, which starts offscreen as at login: wait for its socket,
# check its Settings see autostart enabled, quit. It is stopped on any failure,
# so a retry starts clean.
retry as_test "QT_QPA_PLATFORM=offscreen timeout 120 /usr/libexec/codexbar-autostart &
  trap 'kill \$(jobs -p) 2>/dev/null; wait' EXIT
  for i in \$(seq 60); do codexbar-linux --snapshot >/dev/null 2>&1 && break; sleep 1; done
  codexbar-linux --autostart status | grep -q '\"enabled\":true' && codexbar-linux --quit && wait"

# Agents on a local model through llmman. A turn that runs away (see above)
# fails with the token limit, in about 10 minutes at the 7-8 tokens/s these
# runners generate, and is retried; each sample is independent and a retry
# finds the prompt already in llama-server's cache, so it costs generation time
# only. Two runaways in a row have been seen. The 1800 s budget per attempt
# cannot be tightened: a legitimate Claude Code turn takes up to 25 minutes on
# the arm64 runners, nearly all of it prompt processing of its 20k-token
# system prompt.
if [ "$inference" != 0 ]; then
  # In a container llmman downloads llama.cpp itself (LLMMAN_RUNTIME=bin) and
  # looks the release up through GitHub's REST API with no token: 60 requests
  # an hour per IP address, shared by every job behind the runner's address.
  # That has answered 403 to every try, and a daemon whose fetch failed
  # repeats it on every load, so fetch it once here, before the daemon starts.
  # Waiting out the limit does not help (centos-kde: 403 again after 882 s,
  # and again after 114 s more). Only the lookup is limited; the release's
  # files are plain github.com downloads. So when llmman fails, unpack the
  # release it asked for into its cache and run it again.
  if systemd-detect-virt -cq; then
    # The cache is <tag>/<backend>, complete once the sentinel exists
    seed_llama_cpp() {
      local tag=$1 arch tgz ok=0
      local dest=$llmman_data/llama.cpp/$tag/cpu
      case $(uname -m) in x86_64) arch=x64 ;; aarch64) arch=arm64 ;; *) return 1 ;; esac
      [ ! -e "$dest/.llmman-complete" ] || return 1 # seeded already: not the API
      tgz=$(mktemp)
      curl -fsSL --retry 5 --retry-all-errors --retry-delay 10 -o "$tgz" \
        "https://github.com/ggml-org/llama.cpp/releases/download/$tag/llama-$tag-bin-ubuntu-$arch.tar.gz" &&
        runuser -l test -c "rm -rf '$dest' && mkdir -p '$dest' && tar -xzf - -C '$dest' &&
          touch '$dest/.llmman-complete'" <"$tgz" || ok=1
      rm -f "$tgz"
      return "$ok"
    }
    pull_log=$(mktemp)
    for i in 1 2 3; do
      status=0
      as_test "llmman serve --pull-only" >"$pull_log" 2>&1 || status=$?
      cat "$pull_log"
      [ "$status" -ne 0 ] || break
      [ "$i" -lt 3 ] || exit 1
      # "... api.github.com/repos/ggml-org/llama.cpp/releases/tags/b11429 returned 403"
      tag=
      [[ $(<"$pull_log") =~ releases/tags/(b[0-9]+) ]] && tag=${BASH_REMATCH[1]}
      [ -n "$tag" ] && seed_llama_cpp "$tag" || sleep 30
    done
  fi
  # The pull is a chain of registry requests and has timed out on one
  retry as_test "llmman pull qwen3.5:0.8b"
  prompt="Reply with exactly the word OK and nothing else"
  for c in "opencode -- run '$prompt'" \
           "claude -- -p '$prompt'" \
           "codex -- exec --skip-git-repo-check '$prompt'" \
           "openclaw -- agent --local -m '$prompt'"; do
    agent=${c%% *}
    launch="timeout 1800 llmman launch $agent --model qwen3.5:0.8b"
    if [ "$agent" = openclaw ]; then
      # No tools: the turn only asks for a word, and a 0.8b model that has
      # them calls one with bad arguments (write on a directory, ls on a
      # doubled path) until OpenClaw aborts, on every retry. Deny all, not one
      # group at a time. After llmman has onboarded (a launch that runs no
      # turn): it skips that when openclaw.json exists. tools.deny leaves Tool
      # Search on (tool_search, tool_describe), which the model misuses the
      # same way, so switch it off too. --merge keeps the tools profile.
      retry as_test "$launch -- --version"
      as_test "openclaw config set tools '{\"deny\":[\"*\"],\"toolSearch\":false}' --strict-json --merge"
    fi
    retry as_test "$launch ${c#* }"
  done
fi

# /tmp and /var/tmp are one directory on disk, which nothing empties at
# shutdown the way a tmpfs is: agenticlinux-clean-var-tmp.service does, in its
# ExecStop, which stopping the unit runs. Last, because it also removes the
# PrivateTmp directories of running services. The dotfile, symlink and fifo are
# what rm -rf /var/tmp/* or a confined find would leave behind.
if ! systemd-detect-virt -cq; then
  unit=agenticlinux-clean-var-tmp.service
  systemctl is-enabled "$unit"
  systemctl is-active "$unit"
  # Stops after the bootguard arm step, whose bootc writes to /tmp
  [[ " $(systemctl show -p Before --value "$unit") " == *" agenticlinux-bootguard-arm.service "* ]]
  mkdir -p /var/tmp/clean-test/sub
  touch /var/tmp/clean-test/sub/file /var/tmp/.clean-test-dotfile /tmp/clean-test-file
  ln -s /nonexistent /var/tmp/clean-test/link
  mkfifo /var/tmp/clean-test/fifo
  systemctl stop "$unit"
  for f in /var/tmp/clean-test /var/tmp/.clean-test-dotfile /tmp/clean-test-file; do
    test ! -e "$f"
  done
  # The directory stays: it is the mount point of /tmp
  for d in /var/tmp /tmp; do
    test "$(stat -c %a "$d")" = 1777
  done
fi
echo SMOKE PASS
