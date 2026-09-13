#!/bin/bash
# Runs inside the image build; see Dockerfile.
set -euxo pipefail

case "$(uname -m)" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
esac

FEDORA=$(rpm -E %fedora)

dnf -y install dnf5-plugins \
  "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-${FEDORA}.noarch.rpm" \
  "https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-${FEDORA}.noarch.rpm"
dnf config-manager addrepo --from-repofile=https://download.docker.com/linux/fedora/docker-ce.repo
dnf config-manager addrepo --from-repofile=https://mise.jdx.dev/rpm/mise.repo

# shellcheck disable=SC2046
dnf -y install $(sed 's/#.*//' /tmp/packages.txt) \
  "https://github.com/docker/sbx-releases/releases/latest/download/DockerSandboxes-linux-${ARCH}-rockylinux8.rpm"

curl -fsSL -o /usr/bin/llmman \
  "https://github.com/llmmanorg/llmman/releases/latest/download/llmman-$(uname -m)-unknown-linux-gnu"
chmod 755 /usr/bin/llmman
/usr/bin/llmman --version

systemctl enable docker.service

dnf clean all
rm -rf /var/cache /var/log/* /run/* /tmp/*
