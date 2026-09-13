# Fedora bootc desktop with Docker Engine, Docker Sandboxes, llmman and a
# developer toolset preinstalled.
#
# VARIANT is one of: kinoite silverblue sway-atomic cosmic-atomic
#                    xfce-atomic budgie-atomic base-atomic (no desktop)
ARG VARIANT=base-atomic
ARG FEDORA=44
FROM quay.io/fedora-ostree-desktops/${VARIANT}:${FEDORA}

COPY packages.txt build.sh /tmp/
RUN /tmp/build.sh

RUN bootc container lint
