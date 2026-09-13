# Smoke-test image: the built image plus an unprivileged user and the
# in-VM test script (started via systemd.run=, see test/smoke.sh).
ARG IMAGE
FROM ${IMAGE}
COPY test/smoke-vm.sh /usr/bin/smoke-vm
RUN useradd -m test
