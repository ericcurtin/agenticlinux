#!/bin/bash
# Runs inside the image build; see Dockerfile.
set -euxo pipefail

case "$(uname -m)" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
esac

FEDORA=$(rpm -E %fedora)
KVER=$(rpm -q kernel --qf '%{VERSION}-%{RELEASE}.%{ARCH}')
# /root is a dangling symlink to /var/roothome during the build
export HOME=/tmp

# Patient retries: GitHub Releases has had multi-minute outages mid-build
curl() { command curl -fsSL --retry 12 --retry-delay 20 --retry-all-errors "$@"; }

dnf -y install dnf5-plugins \
  "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-${FEDORA}.noarch.rpm" \
  "https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-${FEDORA}.noarch.rpm"
dnf config-manager addrepo --from-repofile=https://download.docker.com/linux/fedora/docker-ce.repo
dnf config-manager addrepo --from-repofile=https://mise.jdx.dev/rpm/mise.repo
dnf config-manager addrepo --from-repofile=https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo

curl -o /tmp/docker-sbx.rpm \
  "https://github.com/docker/sbx-releases/releases/latest/download/DockerSandboxes-linux-${ARCH}-rockylinux8.rpm"
# shellcheck disable=SC2046
dnf -y install $(sed 's/#.*//' /tmp/packages.txt) "kernel-devel-${KVER}" /tmp/docker-sbx.rpm

# Fedora's ROCm packages are x86_64 only
if [ "$(uname -m)" = x86_64 ]; then
  dnf -y install rocm-hip rocm-opencl rocm-runtime rocm-smi rocminfo rocm-clinfo \
    rocblas hipblas hipblaslt rccl
fi

# /usr is read-only on a bootc host, so build the NVIDIA kernel module now.
# akmod-nvidia's %post only works in rpm-ostree's sandbox; skip it and run
# akmods ourselves, then drop the build-time packages. akmods exits 0 on a
# failed build, hence the modinfo check.
#
# On aarch64 the driver is best effort. RPM Fusion's aarch64 driver breaks in
# ways x86_64's does not (615.71.09-3 requires a Tegra library no package
# provides, so dnf fell back to the GA 595 driver, which no longer builds
# against the current kernel) and an NVIDIA GPU in a Fedora aarch64 desktop is
# rare; holding both architectures' images back for it is not worth it. The
# image then keeps nouveau, which the kernel arguments would otherwise
# blacklist, and says so in the build log.
if dnf -y install --setopt=tsflags=noscripts akmod-nvidia &&
   akmods --force --kernels "${KVER}" &&
   modinfo -k "${KVER}" nvidia >/dev/null; then
  dnf -y install xorg-x11-drv-nvidia-cuda
elif [ "$(uname -m)" = aarch64 ]; then
  tail -n 30 /var/cache/akmods/nvidia/*.failed.log || true
  echo "WARNING: the NVIDIA driver could not be installed, building without it" >&2
  rm /usr/lib/bootc/kargs.d/10-nvidia.toml
else
  exit 1
fi
dnf -y remove akmod-nvidia "kernel-devel-${KVER}"
nvidia-ctk runtime configure --runtime=docker

# Upstream Node bundles its own SQLite; OpenClaw refuses the system SQLite
# Fedora's Node links against (WAL corruption bug in 3.51.2).
case "$(uname -m)" in x86_64) NODE_ARCH=x64 ;; aarch64) NODE_ARCH=arm64 ;; esac
NODE=$(curl https://nodejs.org/dist/index.json |
  python3 -c 'import sys,json; print(next(v["version"] for v in json.load(sys.stdin) if v["version"].startswith("v24") and v["lts"]))')
curl "https://nodejs.org/dist/${NODE}/node-${NODE}-linux-${NODE_ARCH}.tar.xz" |
  tar xJ -C /usr --strip-components=1 --exclude='*/CHANGELOG.md' --exclude='*/LICENSE' --exclude='*/README.md'
npm install -g --prefix /usr @anthropic-ai/claude-code @openai/codex opencode-ai openclaw

curl -o /usr/bin/llmman \
  "https://github.com/llmmanorg/llmman/releases/latest/download/llmman-$(uname -m)-unknown-linux-gnu"
chmod 755 /usr/bin/llmman
/usr/bin/llmman --version

systemctl enable docker.service

dnf clean all
rm -rf /var/cache /var/log/* /run/dnf /tmp/*
