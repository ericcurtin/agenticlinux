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

curl() { command curl -fsSL --retry 5 --retry-all-errors "$@"; }

dnf -y install dnf5-plugins \
  "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-${FEDORA}.noarch.rpm" \
  "https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-${FEDORA}.noarch.rpm"
dnf config-manager addrepo --from-repofile=https://download.docker.com/linux/fedora/docker-ce.repo
dnf config-manager addrepo --from-repofile=https://mise.jdx.dev/rpm/mise.repo
dnf config-manager addrepo --from-repofile=https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo

# shellcheck disable=SC2046
dnf -y install $(sed 's/#.*//' /tmp/packages.txt) "kernel-devel-${KVER}" \
  "https://github.com/docker/sbx-releases/releases/latest/download/DockerSandboxes-linux-${ARCH}-rockylinux8.rpm"

# Fedora's ROCm packages are x86_64 only
if [ "$(uname -m)" = x86_64 ]; then
  dnf -y install rocm-hip rocm-opencl rocm-runtime rocm-smi rocminfo rocm-clinfo \
    rocblas hipblas hipblaslt rccl
fi

# /usr is read-only on a bootc host, so build the NVIDIA kernel module now.
# akmod-nvidia's %post only works in rpm-ostree's sandbox; skip it and run
# akmods ourselves, then drop the build-time packages.
dnf -y install --setopt=tsflags=noscripts akmod-nvidia
akmods --force --kernels "${KVER}"
modinfo -k "${KVER}" nvidia >/dev/null
dnf -y install xorg-x11-drv-nvidia-cuda
dnf -y remove akmod-nvidia "kernel-devel-${KVER}"
nvidia-ctk runtime configure --runtime=docker

npm install -g --prefix /usr @anthropic-ai/claude-code @openai/codex opencode-ai openclaw

curl -o /usr/bin/llmman \
  "https://github.com/llmmanorg/llmman/releases/latest/download/llmman-$(uname -m)-unknown-linux-gnu"
chmod 755 /usr/bin/llmman
/usr/bin/llmman --version

systemctl enable docker.service

dnf clean all
rm -rf /var/cache /var/log/* /run/dnf /tmp/*
