#!/bin/bash
# Build an installer ISO for one image: the upstream network installer ISO
# with a kickstart that pulls the image from the registry, so the ISO stays
# ~1.2 GB instead of embedding the 5-8 GB image. Runs in a fedora:N container;
# see the workflow.
#
# Usage: iso/build.sh <image-ref> <variant> <output.iso>
# REPO_URL (optional): the project's home, for the installer's bug-report URL
# and the logo.
set -euxo pipefail

image=$1
variant=$2
out=$3
arch=$(uname -m)
fedora=$(rpm -E %fedora)
repo_url=${REPO_URL:-https://github.com/ericcurtin/agenticlinux}

# generic-logos: unbranded versions of the installer's sidebar and top bar
# backgrounds, see below (--allowerasing: it conflicts with fedora-logos,
# should the container image ever ship that)
dnf -y install --allowerasing lorax generic-logos

url="https://dl.fedoraproject.org/pub/fedora/linux/releases/${fedora}/Everything/${arch}/iso/"
netinst=$(curl -fsSL "$url" | grep -o "Fedora-Everything-netinst-${arch}-[^\"<]*\.iso" | sort -u | tail -1)
curl -fsSL --retry 5 --retry-all-errors -o netinst.iso "${url}${netinst}"

cat > agenticlinux.ks <<KS
# Installs the bootc image; disk, user and locale are chosen in the installer.
ostreecontainer --url=${image} --transport=registry --no-signature-verification
reboot
KS

# Anaconda auto-applies /images/updates.img from the media, copying it over
# the installer's root. Used for three things:
#
# 1. A single plain xfs root as the default, matching bootc install to-disk.
#    The stock layout adds a /home partition; with PLAIN that is a real GPT
#    partition typed "Linux home", which systemd-gpt-auto-generator then tries
#    to mount at /home (a symlink on ostree) in addition to anaconda's
#    /var/home entry. That mount fails and drops the first boot into
#    emergency mode.
mkdir -p updates/etc/anaconda/conf.d updates/usr/share/anaconda/pixmaps images
cat > updates/etc/anaconda/conf.d/99-agenticlinux.conf <<CONF
[Storage]
file_system_type = xfs
default_scheme = PLAIN
default_partitioning =
    / (min 1 GiB)
CONF
# 2. The product the installer says it is installing (/.buildstamp is where
#    anaconda reads its product name and version from; IsFinal keeps the
#    pre-release warning off).
cat > updates/.buildstamp <<STAMP
[Main]
Product=AgenticLinux
Version=${fedora}
IsFinal=True
BugURL=${repo_url}/issues
STAMP
# 3. The installer's artwork: the logo in the sidebar and the sidebar and top
#    bar backgrounds, all Fedora-branded in the upstream ISO.
curl -fsSL --retry 5 --retry-all-errors -o updates/usr/share/anaconda/pixmaps/sidebar-logo.png \
  "${repo_url}/releases/download/assets/agenticlinux-logo-64.png"
cp /usr/share/anaconda/pixmaps/sidebar-bg.png /usr/share/anaconda/pixmaps/topbar-bg.png \
  updates/usr/share/anaconda/pixmaps/
(cd updates && find . | cpio -o -H newc --quiet | gzip) > images/updates.img

# The volume id names the medium (and mkksiso rewrites the kernel arguments
# that refer to it); the boot menu entries are rewritten in place.
mkksiso --skip-mkefiboot --ks agenticlinux.ks -a images \
  -V "AgenticLinux-${variant}-${arch}" \
  -R "Fedora ${fedora}" "AgenticLinux ${fedora}" \
  -R "a Fedora system" "an AgenticLinux system" \
  netinst.iso "$out"
rm -rf netinst.iso agenticlinux.ks updates images
