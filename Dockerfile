# AgenticLinux: a bootc desktop with Docker Engine, Docker Sandboxes, llmman,
# GPU runtimes (Vulkan, ROCm, NVIDIA/CUDA) and a developer toolset preinstalled.
#
# VARIANT is the upstream image a variant is built from, one of:
#   kinoite silverblue sway-atomic cosmic-atomic xfce-atomic budgie-atomic
#   base-atomic (no desktop)
# REPO_URL is the project's home: the OS identity files point at it and the
# logo is downloaded from its release assets.
ARG VARIANT=base-atomic
ARG FEDORA=44
FROM quay.io/fedora-ostree-desktops/${VARIANT}:${FEDORA}
ARG VARIANT
ARG REPO_URL=https://github.com/ericcurtin/agenticlinux

COPY usr /usr
COPY packages.txt build.sh /tmp/
RUN VARIANT="$VARIANT" REPO_URL="$REPO_URL" /tmp/build.sh

RUN bootc container lint
