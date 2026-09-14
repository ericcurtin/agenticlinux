#!/bin/bash
# Build an installer ISO for one image: Fedora's netinst ISO with a kickstart
# that pulls the image from the registry, so the ISO stays ~1.2 GB instead of
# embedding the 5-8 GB image. Runs in a fedora:N container; see the workflow.
#
# Usage: iso/build.sh <image-ref> <output.iso>
set -euxo pipefail

image=$1
out=$2
arch=$(uname -m)
fedora=$(rpm -E %fedora)

dnf -y install lorax

url="https://dl.fedoraproject.org/pub/fedora/linux/releases/${fedora}/Everything/${arch}/iso/"
netinst=$(curl -fsSL "$url" | grep -o "Fedora-Everything-netinst-${arch}-[^\"<]*\.iso" | sort -u | tail -1)
curl -fsSL --retry 5 --retry-all-errors -o netinst.iso "${url}${netinst}"

cat > agenticlinux.ks <<KS
# Installs the bootc image; disk, user and locale are chosen in the installer.
ostreecontainer --url=${image} --transport=registry --no-signature-verification
reboot
KS

# Anaconda auto-applies /images/updates.img from the media: use it to make
# plain xfs partitions the default, matching bootc install to-disk.
mkdir -p updates/etc/anaconda/conf.d images
printf '[Storage]\nfile_system_type = xfs\ndefault_scheme = PLAIN\n' > updates/etc/anaconda/conf.d/99-agenticlinux.conf
(cd updates && find . | cpio -o -H newc --quiet | gzip) > images/updates.img

mkksiso --skip-mkefiboot --ks agenticlinux.ks -a images netinst.iso "$out"
rm -rf netinst.iso agenticlinux.ks updates images
