#!/bin/bash
# Runs inside the image build; see Dockerfile. VARIANT and REPO_URL come from
# the build arguments.
set -euxo pipefail

case "$(uname -m)" in
  x86_64) ARCH=amd64 ;;
  aarch64) ARCH=arm64 ;;
esac

# What each upstream image becomes; the workflow has the same table
case "$VARIANT" in
  kinoite)       VARIANT_ID=kde    DESKTOP="KDE Plasma" ;;
  silverblue)    VARIANT_ID=gnome  DESKTOP=GNOME ;;
  sway-atomic)   VARIANT_ID=sway   DESKTOP=Sway ;;
  cosmic-atomic) VARIANT_ID=cosmic DESKTOP=COSMIC ;;
  xfce-atomic)   VARIANT_ID=xfce   DESKTOP=Xfce ;;
  budgie-atomic) VARIANT_ID=budgie DESKTOP=Budgie ;;
  base-atomic)   VARIANT_ID=base   DESKTOP="no desktop" ;;
  *) echo "unknown VARIANT '$VARIANT'" >&2; exit 1 ;;
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

# --- Identity ---------------------------------------------------------------
# This is a remix built from Fedora's packages, not Fedora: Fedora's trademark
# guidelines allow the former but not presenting a modified distribution under
# Fedora's name and logo. Fedora ships generic-logos for exactly this, an
# unbranded drop-in for its artwork package (same file names and provides, so
# nothing that wants a system logo breaks). Its release identity files are
# plain text and are overwritten below; the fedora-release packages themselves
# stay, many packages require them by name. Done after every dnf transaction
# so no package update can put the originals back.
if rpm -q fedora-logos >/dev/null; then
  dnf -y swap fedora-logos generic-logos
fi
# Fedora's first-run app and its Fedora-logo icon theme override, where present
for p in plasma-welcome-fedora breeze-icon-theme-fedora; do
  if rpm -q "$p" >/dev/null; then dnf -y remove "$p"; fi
done

# The logo is not in git; it is a release asset of the repository
mkdir -p /tmp/logo
sizes="16 22 24 32 48 64 128 256 512"
files=agenticlinux-logo.svg
for s in $sizes; do files="$files agenticlinux-logo-$s.png"; done
for f in $files; do
  curl -o "/tmp/logo/$f" "${REPO_URL}/releases/download/assets/$f"
done
install -Dm644 /tmp/logo/agenticlinux-logo.svg /usr/share/icons/hicolor/scalable/apps/agenticlinux.svg
for s in $sizes; do
  install -Dm644 "/tmp/logo/agenticlinux-logo-$s.png" "/usr/share/icons/hicolor/${s}x${s}/apps/agenticlinux.png"
done
install -Dm644 /tmp/logo/agenticlinux-logo.svg /usr/share/pixmaps/agenticlinux-logo.svg
install -Dm644 /tmp/logo/agenticlinux-logo-256.png /usr/share/pixmaps/agenticlinux-logo.png

# generic-logos keeps fedora-logos' file names but fills them with placeholder
# art (a dancing hot dog), and those names are what the desktops ask for:
# Plasma's launcher icon is "start-here" (kde-settings), the system logo icon
# is "fedora-logo-icon", anaconda and the login greeters read the pixmaps.
# Put the AgenticLinux logo behind every one of those names.
if rpm -q generic-logos >/dev/null; then
  for f in $(rpm -ql generic-logos | grep -E '/(fedora-logo|start-here|system-logo)[^/]*\.(png|svg)$'); do
    [ -L "$f" ] && continue
    case "$f" in
      *.svg) install -m644 /tmp/logo/agenticlinux-logo.svg "$f" ;;
      *-small.png) install -m644 /tmp/logo/agenticlinux-logo-128.png "$f" ;;
      *.png) install -m644 /tmp/logo/agenticlinux-logo-256.png "$f" ;;
    esac
  done
fi
# GTK trusts an icon cache that is newer than its directory, so refresh it
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -f -t -q /usr/share/icons/hicolor
else
  touch /usr/share/icons/hicolor
fi

# GNOME's background-logo extension is preset to fedora-logos' files
schemas=/usr/share/glib-2.0/schemas
if [ -e "$schemas/org.fedorahosted.background-logo-extension.gschema.xml" ]; then
  cat > "$schemas/zz-agenticlinux-background-logo.gschema.override" <<EOF
[org.fedorahosted.background-logo-extension]
logo-file='/usr/share/pixmaps/agenticlinux-logo.svg'
logo-file-dark='/usr/share/pixmaps/agenticlinux-logo.svg'
EOF
  glib-compile-schemas "$schemas"
fi

# --- Wallpaper --------------------------------------------------------------
# The default wallpaper is Fedora's release artwork, wired up by the
# desktop-backgrounds packages: gsettings overrides for GNOME and Budgie,
# /usr/share/wallpapers/Default for Plasma (its look-and-feel and lock screen
# are preset to it), and /usr/share/backgrounds/default*.jxl for Sway, Xfce,
# COSMIC and the greeters. Render the AgenticLinux wallpaper and point each
# of those at it. The renderers are build-time only. The compat links keep
# their names (Fedora's are JPEG XL), so the image is encoded as both.
bg=/usr/share/backgrounds/agenticlinux
compat=$(ls /usr/share/backgrounds/default.* /usr/share/backgrounds/default-dark.* \
            /usr/share/backgrounds/images/default*.* 2>/dev/null || true)
if [ -n "$compat" ] || [ -d /usr/share/wallpapers ] ||
   [ -e "$schemas/org.gnome.desktop.background.gschema.xml" ]; then
  tools=""
  for p in librsvg2-tools libjxl-utils; do rpm -q "$p" >/dev/null || tools="$tools $p"; done
  # shellcheck disable=SC2086
  [ -z "$tools" ] || dnf -y install $tools
  install -m644 /tmp/logo/agenticlinux-logo.svg "$bg/agenticlinux-logo.svg"
  rsvg-convert -w 3840 -h 2160 -o "$bg/agenticlinux.png" "$bg/agenticlinux.svg"
  cjxl -d 0 --quiet "$bg/agenticlinux.png" "$bg/agenticlinux.jxl"
  # shellcheck disable=SC2086
  [ -z "$tools" ] || dnf -y remove $tools

  for f in $compat; do
    case "$f" in
      *.jxl) ln -sf "$bg/agenticlinux.jxl" "$f" ;;
      *.png) ln -sf "$bg/agenticlinux.png" "$f" ;;
    esac
  done
  if [ -L /usr/share/backgrounds/default.xml ] || [ -e /usr/share/backgrounds/default.xml ]; then
    cat > "$bg/agenticlinux.xml" <<EOF
<background>
  <static>
    <duration>86400.0</duration>
    <file>$bg/agenticlinux.jxl</file>
  </static>
</background>
EOF
    ln -sf "$bg/agenticlinux.xml" /usr/share/backgrounds/default.xml
  fi

  if [ -e "$schemas/org.gnome.desktop.background.gschema.xml" ]; then
    cat > "$schemas/zz-agenticlinux-background.gschema.override" <<EOF
[org.gnome.desktop.background]
picture-uri='file://$bg/agenticlinux.jxl'
picture-uri-dark='file://$bg/agenticlinux.jxl'

[org.gnome.desktop.screensaver]
picture-uri='file://$bg/agenticlinux.jxl'
EOF
    if [ -e "$schemas/x.dm.slick-greeter.gschema.xml" ]; then
      cat >> "$schemas/zz-agenticlinux-background.gschema.override" <<EOF

[x.dm.slick-greeter]
background='$bg/agenticlinux.jxl'
EOF
    fi
    glib-compile-schemas "$schemas"
  fi

  if [ -d /usr/share/wallpapers ]; then
    install -d /usr/share/wallpapers/AgenticLinux/contents/images
    ln -sf "$bg/agenticlinux.png" /usr/share/wallpapers/AgenticLinux/contents/images/3840x2160.png
    ln -sf "$bg/agenticlinux.png" /usr/share/wallpapers/AgenticLinux/contents/screenshot.png
    cat > /usr/share/wallpapers/AgenticLinux/metadata.json <<EOF
{
  "KPlugin": {
    "Id": "AgenticLinux",
    "Name": "AgenticLinux",
    "License": "MIT"
  }
}
EOF
    if [ -L /usr/share/wallpapers/Default ]; then
      ln -sfn AgenticLinux /usr/share/wallpapers/Default
    fi
  fi
fi

# Fedora's Plasma look-and-feel stays (kde-settings and the Plasma defaults
# name it); only what it shows changes: its wallpaper above, its name here
for f in /usr/share/plasma/look-and-feel/org.fedoraproject.fedora*.desktop/metadata.json; do
  [ -e "$f" ] || continue
  sed -i -e 's/"Name": "Fedora/"Name": "AgenticLinux/' \
         -e 's/"Description": "[^"]*"/"Description": "AgenticLinux theme"/' "$f"
done

# The boot splash's watermark was fedora-logos' too, and it is baked into the
# initramfs, so that is rebuilt the way rpm-ostree builds it for these images
# (their dracut.conf.d already says hostonly=no).
if [ -d /usr/share/plymouth/themes/spinner ]; then
  install -m644 /tmp/logo/agenticlinux-logo-64.png /usr/share/plymouth/themes/spinner/watermark.png
  dracut --no-hostonly --kver "$KVER" --reproducible --add ostree --tmpdir /tmp -f /tmp/initramfs.img
  install -m600 /tmp/initramfs.img "/usr/lib/modules/$KVER/initramfs.img"
fi

cat > /usr/lib/os-release <<EOF
NAME="AgenticLinux"
VERSION="${FEDORA} (${DESKTOP})"
ID=agenticlinux
ID_LIKE=fedora
VERSION_ID=${FEDORA}
PRETTY_NAME="AgenticLinux ${FEDORA} (${DESKTOP})"
ANSI_COLOR="0;38;2;124;108;248"
LOGO=agenticlinux
CPE_NAME="cpe:/o:agenticlinux:agenticlinux:${FEDORA}"
DEFAULT_HOSTNAME="agenticlinux"
HOME_URL="${REPO_URL}"
DOCUMENTATION_URL="${REPO_URL}#readme"
SUPPORT_URL="${REPO_URL}/issues"
BUG_REPORT_URL="${REPO_URL}/issues"
VARIANT="${DESKTOP}"
VARIANT_ID=${VARIANT_ID}
EOF
# /etc/system-release, /etc/redhat-release and /etc/fedora-release link here
echo "AgenticLinux release ${FEDORA}" > /usr/lib/fedora-release

systemctl enable docker.service

dnf clean all
rm -rf /var/cache /var/log/* /run/dnf /tmp/*
