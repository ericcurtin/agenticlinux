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
# logo is downloaded from its release assets. VERSION is the version bootc
# status shows.
#
# The default target has over 250 layers, too many for Docker's overlay2
# store. The published one is the chunked target, the same image repacked
# (rechunk.sh) as an OCI layout:
#   docker build --target chunked -o type=tar,dest=image.tar . &&
#     docker load -i image.tar
ARG BASE=quay.io/fedora-ostree-desktops/base-atomic:44
# rpm-ostree for the repacking; CentOS's hangs in a container build
ARG RECHUNKER=quay.io/fedora/fedora-bootc:44

# bootguard (automatic rollback of a failed update) is built on the variant's own
# base, so it links against the distribution it runs on. dnf gets the patience
# build.sh gives it, including fresh metadata for every retry (stale metadata
# fails the same way each time); cargo has its own retries for crates.io. HOME
# is /tmp as in build.sh: /root is a dangling symlink in these images, so cargo
# cannot use it.
FROM ${BASE} AS bootguard
RUN for i in $(seq 12); do dnf install -y cargo && break; \
      [ "$i" -lt 12 ] || exit 1; echo "dnf failed, retrying in 20 s ($i/12)" >&2; \
      dnf clean metadata >&2; sleep 20; \
    done
ENV HOME=/tmp CARGO_NET_RETRY=12
COPY bootguard/Cargo.toml bootguard/Cargo.lock /src/
COPY bootguard/src /src/src
RUN cd /src && cargo test --locked && cargo build --release --locked

FROM ${BASE} AS image
ARG VARIANT=base
ARG REPO_URL=https://github.com/ericcurtin/agenticlinux

COPY usr /usr
COPY packages.txt build.sh /tmp/
RUN VARIANT="$VARIANT" REPO_URL="$REPO_URL" /tmp/build.sh

COPY --from=bootguard /src/target/release/agenticlinux-bootguard /usr/bin/agenticlinux-bootguard
COPY bootguard/dist /
RUN systemctl enable agenticlinux-bootguard-boot.service \
                     agenticlinux-bootguard-check.service \
                     agenticlinux-bootguard-arm.service

RUN bootc container lint

FROM ${RECHUNKER} AS rechunk
ARG VARIANT=base
ARG VERSION=
RUN --mount=type=bind,from=image,target=/rootfs \
    --mount=type=bind,source=rechunk.sh,target=/tmp/rechunk.sh \
    VARIANT="$VARIANT" VERSION="$VERSION" /tmp/rechunk.sh

FROM scratch AS chunked
COPY --from=rechunk /out /

FROM image
