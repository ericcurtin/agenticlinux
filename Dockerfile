# AgenticLinux: a bootc desktop with Docker Engine, Docker Sandboxes, llmman,
# GPU runtimes (Vulkan, ROCm, NVIDIA/CUDA) and a developer toolset preinstalled.
#
# BASE is the upstream bootc image a variant is built from and VARIANT the
# variant's name (the public tag). The pairs, also in the workflow's table:
#   kde gnome sway cosmic xfce budgie base
#     quay.io/fedora-ostree-desktops/{kinoite,silverblue,sway-atomic,
#     cosmic-atomic,xfce-atomic,budgie-atomic,base-atomic}:<fedora>
#   centos-kde centos-gnome centos-base
#     quay.io/centos-bootc/centos-bootc:stream10 (the desktop is installed
#     by build.sh; CentOS has no desktop images)
# REPO_URL is the project's home: the OS identity files point at it and the
# logo is downloaded from its release assets.
ARG BASE=quay.io/fedora-ostree-desktops/base-atomic:44
FROM ${BASE}
ARG VARIANT=base
ARG REPO_URL=https://github.com/ericcurtin/agenticlinux

COPY usr /usr
COPY packages.txt build.sh /tmp/
RUN VARIANT="$VARIANT" REPO_URL="$REPO_URL" /tmp/build.sh

RUN bootc container lint
