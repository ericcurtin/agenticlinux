#!/bin/bash
# Runs inside the image build; see Dockerfile. VARIANT and REPO_URL come from
# the build arguments.
set -euxo pipefail

case "$(uname -m)" in
  x86_64)  ARCH=amd64 NODE_ARCH=x64 ;;
  aarch64) ARCH=arm64 NODE_ARCH=arm64 ;;
esac

# The base image sets the distribution, the variant the desktop; the workflow
# has the same table. os-release is read in a subshell: it sets VARIANT too.
read -r DISTRO RELEASE PLATFORM < <(. /usr/lib/os-release; echo "$ID $VERSION_ID ${PLATFORM_ID:-}")
case "$VARIANT" in
  kde|centos-kde)     DESKTOP="KDE Plasma" ;;
  gnome|centos-gnome) DESKTOP=GNOME ;;
  sway)               DESKTOP=Sway ;;
  cosmic)             DESKTOP=COSMIC ;;
  xfce)               DESKTOP=Xfce ;;
  budgie)             DESKTOP=Budgie ;;
  base|centos-base)   DESKTOP="no desktop" ;;
  *) echo "unknown VARIANT '$VARIANT'" >&2; exit 1 ;;
esac
case "$VARIANT" in centos-*) want=centos ;; *) want=fedora ;; esac
[ "$DISTRO" = "$want" ] || { echo "VARIANT '$VARIANT' needs a $want base image, not $DISTRO" >&2; exit 1; }

# /root is a dangling symlink to /var/roothome during the build
export HOME=/tmp

# Patient retries: GitHub Releases has had multi-minute outages mid-build
curl() { command curl -fsSL --retry 12 --retry-delay 20 --retry-all-errors "$@"; }
# And dnf the same. Its own retries are immediate and per mirror, so a stall
# on a single-host repository (nvidia.github.io fed one package at under 1000
# bytes/s for 30 s, four times in a row) fails the transaction. Nothing is
# installed until every package has downloaded, so re-running is safe.
dnf() {
  local i
  for i in $(seq 12); do
    command dnf "$@" && return
    [ "$i" -lt 12 ] || return 1
    echo "dnf failed, retrying in 20 s ($i/12)" >&2
    sleep 20
  done
}

KVER=$(rpm -q kernel --qf '%{VERSION}-%{RELEASE}.%{ARCH}')
read -r kv kr < <(rpm -q kernel --qf '%{VERSION} %{RELEASE}\n')
# What differs by distribution: RPM Fusion's branch, the NVIDIA driver (the
# EL branch has it as a versioned stream), ID_LIKE, and where the kernel-devel
# matching the kernel comes from. akmods requires it, and left to dnf the
# repositories' newest comes with its kernel as a second one. Both images can
# be built ahead of the mirrors: CentOS Stream's always is, and Fedora's picks
# up a kernel the day it goes stable, while some mirrors still serve the
# metadata from before (kernel-devel-7.2.6-200.fc44 was "No match" on one x86_64
# build and found on the rest). The build systems keep every build, so the
# packages come from there when the mirrors don't have them. That is
# kernel-devel-matched as well as kernel-devel: akmods requires the matched
# one for the installed kernel, and a mirror can lack it too, including one
# already past the kernel (7.2.7 in updates, 7.2.6 not yet in updates-archive).
case "$DISTRO" in
  fedora)
    RPMFUSION=fedora NVIDIA=akmod-nvidia CUDA=xorg-x11-drv-nvidia-cuda ID_LIKE=fedora
    koji="https://kojipkgs.fedoraproject.org/packages/kernel/${kv}/${kr}/$(uname -m)"
    KDEVEL=""
    for p in kernel-devel kernel-devel-matched; do
      if [ -n "$(dnf -q repoquery --available "${p}-${KVER}")" ]; then
        KDEVEL="$KDEVEL ${p}-${KVER}"
      else
        KDEVEL="$KDEVEL $koji/${p}-${KVER}.rpm"
      fi
    done
    ;;
  centos)
    RPMFUSION=el NVIDIA=akmod-nvidia-580xx CUDA=xorg-x11-drv-nvidia-580xx-cuda ID_LIKE="rhel centos fedora"
    koji="https://kojihub.stream.centos.org/kojifiles/packages/kernel/${kv}/${kr}/$(uname -m)"
    KDEVEL="$koji/kernel-devel-${KVER}.rpm $koji/kernel-devel-matched-${KVER}.rpm"
    ;;
esac

# --- Repositories -----------------------------------------------------------
# CentOS: EPEL for the desktops beyond GNOME and most of the tools, CRB for
# what EPEL builds against
if [ "$DISTRO" = centos ]; then
  dnf -y install epel-release
  crb enable
fi
dnf -y install \
  "https://mirrors.rpmfusion.org/free/${RPMFUSION}/rpmfusion-free-release-${RELEASE}.noarch.rpm" \
  "https://mirrors.rpmfusion.org/nonfree/${RPMFUSION}/rpmfusion-nonfree-release-${RELEASE}.noarch.rpm"
for r in "https://download.docker.com/linux/${DISTRO}/docker-ce.repo" \
         https://mise.jdx.dev/rpm/mise.repo \
         https://nvidia.github.io/libnvidia-container/stable/rpm/nvidia-container-toolkit.repo; do
  curl -o "/etc/yum.repos.d/${r##*/}" "$r"
done

# --- Desktop (CentOS) -------------------------------------------------------
# CentOS's bootc image has no desktop, so it is a group install: GNOME from
# CentOS, KDE Plasma from EPEL, plus the groups their installer environments
# add. Not Core and Standard: the image is complete already, and they pin
# tools to package versions the image is ahead of, failing the transaction.
if [ "$DISTRO" = centos ]; then
  case "$VARIANT" in
    centos-gnome) groups="gnome-desktop workstation-product" ;;
    centos-kde)   groups="kde-desktop" ;;
    *)            groups="" ;;
  esac
  if [ -n "$groups" ]; then
    # shellcheck disable=SC2086
    dnf -y group install base-graphical fonts input-methods multimedia hardware-support \
      guest-desktop-agents networkmanager-submodules desktop-accessibility $groups
  fi
fi

# --- Packages ---------------------------------------------------------------
# Docker Sandboxes from its nightly build of main (a rolling release tag, the
# same asset names as the tagged releases)
curl -o /tmp/docker-sbx.rpm \
  "https://github.com/docker/sbx-releases/releases/download/nightly/DockerSandboxes-linux-${ARCH}-rockylinux8.rpm"
# shellcheck disable=SC2046,SC2086
dnf -y install $(sed 's/#.*//' /tmp/packages.txt) $KDEVEL /tmp/docker-sbx.rpm

# ROCm is x86_64 only, in Fedora and in EPEL (which has no OpenCL packages)
if [ "$(uname -m)" = x86_64 ]; then
  rocm="rocm-hip rocm-runtime rocm-smi rocminfo rocblas hipblas hipblaslt rccl"
  [ "$DISTRO" = centos ] || rocm="$rocm rocm-opencl rocm-clinfo"
  # shellcheck disable=SC2086
  dnf -y install $rocm
fi

# /usr is read-only on a bootc host, so build the NVIDIA kernel module now.
# akmod-nvidia's %post only works in rpm-ostree's sandbox; skip it and run
# akmods ourselves, then drop the build-time packages. akmods exits 0 on a
# failed build, hence the modinfo check.
#
# On x86_64 the driver is required: one that stops building fails the build
# rather than quietly shipping nouveau. On aarch64 it is best effort: RPM
# Fusion's aarch64 driver breaks in ways x86_64's does not (615.71.09-3 needs
# a Tegra library nothing provides, and the 595 dnf falls back to no longer
# builds) and an NVIDIA GPU in an aarch64 desktop is rare. The image then
# keeps nouveau, which the kernel arguments would otherwise blacklist.
nvidia_install() {
  dnf -y install --setopt=tsflags=noscripts "$NVIDIA" &&
    akmods --force --kernels "${KVER}" &&
    modinfo -k "${KVER}" nvidia >/dev/null &&
    dnf -y install "$CUDA"
}
if nvidia_install; then
  :
elif [ "$(uname -m)" = aarch64 ]; then
  tail -n 30 /var/cache/akmods/*/*.failed.log || true
  echo "WARNING: the NVIDIA driver could not be installed, building without it" >&2
  rm /usr/lib/bootc/kargs.d/10-nvidia.toml
else
  exit 1
fi
for p in "$NVIDIA" "kernel-devel-${KVER}"; do
  if rpm -q "$p" >/dev/null; then dnf -y remove "$p"; fi
done
nvidia-ctk runtime configure --runtime=docker

# --- Agents -----------------------------------------------------------------
# Upstream Node bundles its own SQLite; OpenClaw refuses the system SQLite the
# distribution's Node links against (WAL corruption bug in 3.51.2).
NODE=$(curl https://nodejs.org/dist/index.json |
  python3 -c 'import sys,json; print(next(v["version"] for v in json.load(sys.stdin) if v["version"].startswith("v24") and v["lts"]))')
curl "https://nodejs.org/dist/${NODE}/node-${NODE}-linux-${NODE_ARCH}.tar.xz" |
  tar xJ -C /usr --strip-components=1 --no-same-owner \
    --exclude='*/CHANGELOG.md' --exclude='*/LICENSE' --exclude='*/README.md'
npm install -g --prefix /usr @anthropic-ai/claude-code @openai/codex opencode-ai openclaw

curl -o /usr/bin/llmman \
  "https://github.com/llmmanorg/llmman/releases/latest/download/llmman-$(uname -m)-unknown-linux-gnu"
chmod 755 /usr/bin/llmman
/usr/bin/llmman --version

# herdr, the terminal workspace the agents run in: a static binary per
# architecture, released the way llmman is
curl -o /usr/bin/herdr \
  "https://github.com/herdrdev/herdr/releases/latest/download/herdr-linux-$(uname -m)"
chmod 755 /usr/bin/herdr
/usr/bin/herdr --version

# --- Desktop apps -----------------------------------------------------------
# The agents' desktop apps that come as RPMs: ChatGPT (Codex is part of it)
# and OpenCode. Claude's is a .deb only.
curl -o /tmp/chatgpt.rpm \
  "https://persistent.oaistatic.com/codex-app-prod/linux/rpm/latest/chatgpt.$(uname -m).rpm"
curl -o /tmp/opencode-desktop.rpm \
  "https://github.com/anomalyco/opencode/releases/latest/download/opencode-desktop-linux-$(uname -m).rpm"
# OpenCode's RPM installs into /opt, which bootc keeps in the mutable /var
# (there after the first install, never updated), so it moves under /usr.
# Fedora's /opt is a dangling link; rpm needs its target to exist.
install -d "$(readlink -f /opt)"
dnf -y install /tmp/chatgpt.rpm /tmp/opencode-desktop.rpm
# Updates come with the image; ChatGPT's repository would be one more host
# for every dnf call to reach
sed -i 's/^enabled=1/enabled=0/' /etc/yum.repos.d/chatgpt.repo
mv /opt/OpenCode /usr/lib/opencode-desktop
sed -i 's|/opt/OpenCode/|/usr/lib/opencode-desktop/|' \
  /usr/share/applications/opencode-desktop.desktop /usr/share/applications/ai.opencode.desktop.desktop
# The %post's alternatives entry and the build-id links point into /opt too
update-alternatives --remove ai.opencode.desktop /opt/OpenCode/ai.opencode.desktop || true
ln -sfn ../lib/opencode-desktop/ai.opencode.desktop /usr/bin/ai.opencode.desktop
ln -sfn ../lib/opencode-desktop/ai.opencode.desktop /usr/bin/opencode-desktop
find /usr/lib/.build-id -lname '*/opt/OpenCode/*' | while read -r l; do
  ln -sfn "$(readlink "$l" | sed 's|/opt/OpenCode/|/usr/lib/opencode-desktop/|')" "$l"
done

# --- Browser ----------------------------------------------------------------
# Google Chrome from Google's repository (signed, one per architecture; the
# RPM's %post would write the same file). Chrome installs into /opt like
# OpenCode and moves under /usr the same way: its launcher finds its files
# through readlink -f, the menu entries go through
# /usr/bin/google-chrome-stable, and only GNOME's default-apps entry names the
# directory. The repository and the daily cron job that re-adds it are how
# Google updates a mutable system; here updates come with the image.
cat > /etc/yum.repos.d/google-chrome.repo <<EOF
[google-chrome]
name=google-chrome
baseurl=https://dl.google.com/linux/chrome/rpm/stable/$(uname -m)
enabled=1
gpgcheck=1
gpgkey=https://dl.google.com/linux/linux_signing_key.pub
EOF
dnf -y install google-chrome-stable
sed -i 's/^enabled=1/enabled=0/' /etc/yum.repos.d/google-chrome.repo
rm /etc/cron.daily/google-chrome
mv /opt/google/chrome /usr/lib/google-chrome
rmdir /opt/google
ln -sfn ../lib/google-chrome/google-chrome /usr/bin/google-chrome-stable
sed -i 's|/opt/google/chrome/|/usr/lib/google-chrome/|' \
  /usr/share/gnome-control-center/default-apps/google-chrome.xml

# --- Identity ---------------------------------------------------------------
# A remix of Fedora's or CentOS's packages, not Fedora or CentOS: their
# trademark guidelines allow that, not a modified distribution under their
# name and logo. Fedora's generic-logos is an unbranded drop-in for its
# artwork package (same file names and provides); CentOS has none, so
# centos-logos stays and its artwork is overwritten below. The release files
# are plain text and overwritten too; the release packages stay, many
# packages require them. Done after every dnf transaction so no update can
# put the originals back.
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

# generic-logos keeps fedora-logos' file names with placeholder art (a
# dancing hot dog), centos-logos has CentOS's under mostly the same names,
# and those names are what the desktops ask for: Plasma's launcher icon is
# "start-here", the system logo icon "fedora-logo-icon", anaconda, GDM and
# the greeters read the pixmaps. Put the AgenticLinux logo behind every one,
# at the size the icon directory names.
for p in generic-logos centos-logos; do
  rpm -q "$p" >/dev/null || continue
  for f in $(rpm -ql "$p" | grep -E '/(fedora[-_]logo|fedora-gdm-logo|centos[-_]logo|start-here|system-logo|bootlogo)[^/]*\.(png|svg)$'); do
    [ -L "$f" ] && continue
    case "$f" in
      *.svg) install -m644 /tmp/logo/agenticlinux-logo.svg "$f" ;;
      *.png)
        s=256
        [[ $f =~ /([0-9]+)x[0-9]+/ ]] && s=${BASH_REMATCH[1]}
        [[ $f =~ _([0-9]+)\.png$ ]] && s=${BASH_REMATCH[1]}
        [[ $f == *-small.png ]] && s=128
        case " $sizes " in *" $s "*) ;; *) s=256 ;; esac
        install -m644 "/tmp/logo/agenticlinux-logo-$s.png" "$f"
        ;;
    esac
  done
done
# GTK trusts an icon cache that is newer than its directory, so refresh it
if command -v gtk-update-icon-cache >/dev/null; then
  gtk-update-icon-cache -f -t -q /usr/share/icons/hicolor
else
  touch /usr/share/icons/hicolor
fi

# GNOME's background-logo extension is preset to the distribution's logo
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
# The default wallpaper is the distribution's release artwork, wired up by
# its backgrounds packages: gsettings overrides for GNOME and Budgie,
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
    "License": "Apache-2.0"
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

# The boot splash's watermark was the logo package's too, and it is baked into
# the initramfs, so that is rebuilt the way the bootc images build it (their
# dracut.conf.d already says hostonly=no).
if [ -d /usr/share/plymouth/themes/spinner ]; then
  install -m644 /tmp/logo/agenticlinux-logo-64.png /usr/share/plymouth/themes/spinner/watermark.png
  dracut --no-hostonly --kver "$KVER" --reproducible --add ostree --tmpdir /tmp -f /tmp/initramfs.img
  install -m600 /tmp/initramfs.img "/usr/lib/modules/$KVER/initramfs.img"
fi

cat > /usr/lib/os-release <<EOF
NAME="AgenticLinux"
VERSION="${RELEASE} (${DESKTOP})"
ID=agenticlinux
ID_LIKE="${ID_LIKE}"
VERSION_ID=${RELEASE}
PRETTY_NAME="AgenticLinux ${RELEASE} (${DESKTOP})"
ANSI_COLOR="0;38;2;124;108;248"
LOGO=agenticlinux
CPE_NAME="cpe:/o:agenticlinux:agenticlinux:${RELEASE}"
DEFAULT_HOSTNAME="agenticlinux"
HOME_URL="${REPO_URL}"
DOCUMENTATION_URL="${REPO_URL}#readme"
SUPPORT_URL="${REPO_URL}/issues"
BUG_REPORT_URL="${REPO_URL}/issues"
VARIANT="${DESKTOP}"
VARIANT_ID=${VARIANT}
EOF
# dnf reads the platform module stream from here
[ -z "$PLATFORM" ] || echo "PLATFORM_ID=\"${PLATFORM}\"" >> /usr/lib/os-release
# The release file /etc/system-release and /etc/redhat-release link to (under
# /usr/lib on Fedora, /etc/centos-release itself on CentOS), and the CPE file
for f in /usr/lib/fedora-release /etc/centos-release; do
  [ -f "$f" ] && [ ! -L "$f" ] && echo "AgenticLinux release ${RELEASE}" > "$f"
done
for f in /usr/lib/system-release-cpe /etc/system-release-cpe; do
  [ -f "$f" ] && [ ! -L "$f" ] && echo "cpe:/o:agenticlinux:agenticlinux:${RELEASE}" > "$f"
done

# CentOS's image had no display manager until the group install. Presets
# enable GDM and SDDM; Plasma's own login manager has none.
#
# The default target is set where Fedora's desktop images set theirs, in
# /usr/lib, not with systemctl set-default: that writes
# /etc/systemd/system/default.target, which outranks the generator directory
# systemd-run-generator redirects default.target from, so the smoke test's
# systemd.run= would be ignored and the guest would boot to the login prompt.
if [ "$DISTRO" = centos ] && [ "$DESKTOP" != "no desktop" ]; then
  ln -sfn graphical.target /usr/lib/systemd/system/default.target
  rm -f /etc/systemd/system/default.target
  if [ ! -e /etc/systemd/system/display-manager.service ]; then
    for dm in gdm plasmalogin sddm; do
      [ -e "/usr/lib/systemd/system/$dm.service" ] || continue
      systemctl enable "$dm.service"
      break
    done
  fi
fi

systemctl enable docker.service

# /tmp was HOME for the build, so npm's cache and the like are dotfiles there.
# Not /run/*: BuildKit bind-mounts its resolv.conf where the image's
# /etc/resolv.conf points, /run/systemd/resolve/stub-resolv.conf, and rm on
# a mountpoint fails the build. /run is a tmpfs on the host anyway.
dnf clean all
rm -rf /var/cache /var/lib/dnf /var/log/* /run/akmods /run/dnf /tmp/* /tmp/.[!.]*
