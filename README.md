# agenticlinux

Fedora 44 [bootc](https://bootc-dev.github.io/bootc/) desktops built on
[fedora-ostree-desktops](https://quay.io/organization/fedora-ostree-desktops)
with [Docker Engine](https://docs.docker.com/engine/),
[Docker Sandboxes](https://docs.docker.com/ai/sandboxes/),
[llmman](https://github.com/llmmanorg/llmman), the `claude`, `codex`,
`opencode` and `openclaw` agents, GPU runtimes (Vulkan, ROCm, NVIDIA/CUDA) and
a developer toolset preinstalled. x86_64 and aarch64.

| Variant    | Desktop       | Image                                   |
|------------|---------------|-----------------------------------------|
| kinoite    | KDE Plasma    | `docker.io/ericcurtin044/agenticlinux:kinoite`    |
| silverblue | GNOME         | `docker.io/ericcurtin044/agenticlinux:silverblue` |
| sway       | Sway          | `docker.io/ericcurtin044/agenticlinux:sway`       |
| cosmic     | COSMIC        | `docker.io/ericcurtin044/agenticlinux:cosmic`     |
| xfce       | Xfce          | `docker.io/ericcurtin044/agenticlinux:xfce`       |
| budgie     | Budgie        | `docker.io/ericcurtin044/agenticlinux:budgie`     |
| base       | none          | `docker.io/ericcurtin044/agenticlinux:base`       |

Every push to `main` builds all variants, boots each one in a VM and smoke
tests Docker, Podman, Docker Sandboxes, llmman and the agents, including
`llmman launch {opencode,claude,codex,openclaw} --model qwen3.5:0.8b`
answering a prompt end to end on a local model. Every disk is booted with
hardware virtualization on Linux (KVM), macOS Intel (HVF) and Windows (WHPX)
runners; there is no emulation fallback. Only if everything passes are the images pushed to
[Docker Hub](https://hub.docker.com/r/ericcurtin044/agenticlinux) and an
installer ISO per variant and architecture (built by [iso/build.sh](iso/build.sh))
published on [GitHub Releases](https://github.com/ericcurtin/agenticlinux/releases).
Images are also tagged `<variant>-<release>` and `<variant>-<release>-<arch>`.

The same smoke test also runs in a plain container of every image on both
architectures (with `llmman serve --runtime bin` and without the docker and
podman checks, which would need nested containers). The aarch64 images are not
boot-tested in CI: no GitHub-hosted arm64 runner can run a VM, and the tests
are never run under emulation. They are published together with the x86_64
images once every build and test is green; the VM test does pass on aarch64
under HVF on Apple silicon.

## Install

Download the ISO for your variant and architecture from the latest release and
boot it. It is Fedora's network installer preset to pull the matching image from
Docker Hub, so the install needs a network connection; disk, user and locale are
chosen in the installer as usual, with plain xfs partitions as the default.

Or switch an existing Fedora Atomic / bootc system:

```sh
sudo bootc switch docker.io/ericcurtin044/agenticlinux:kinoite
```

The root filesystem (which holds `/var`, `/home` and `/root`) defaults to xfs
for both `bootc install` and the ISO.

After the first boot, add yourself to the `docker` and `kvm` groups
(Docker Sandboxes need `/dev/kvm`):

```sh
sudo usermod -aG docker,kvm "$USER"
```

## GPUs

- Vulkan: Mesa drivers and `vulkaninfo`.
- ROCm (x86_64): HIP runtime, OpenCL, rocBLAS, hipBLAS, hipBLASLt, RCCL,
  `rocminfo`, `rocm-smi`. Containers get GPU access with
  `--device /dev/kfd --device /dev/dri`.
- NVIDIA: the RPM Fusion driver with the kernel module prebuilt for the
  image's kernel, CUDA driver libraries and `nvidia-container-toolkit`
  registered with Docker (`docker run --gpus all ...`). The module is
  unsigned, so disable Secure Boot or enroll your own MOK. nouveau is
  blacklisted via kernel arguments. On aarch64 the driver is best effort:
  when RPM Fusion's aarch64 build is broken the image is published without
  it (and with nouveau), see [build.sh](build.sh).

Prebuilt llama.cpp and vLLM (wheels or containers) bundle their own CUDA and
ROCm user-space libraries; the host side above is what they need.

## What's inside

See [packages.txt](packages.txt) and [build.sh](build.sh). Extra
repositories used: RPM Fusion, Docker's Fedora repo, mise's rpm repo and
NVIDIA's container toolkit repo. Docker Sandboxes and llmman are installed
from their GitHub releases; Node from nodejs.org (Fedora's links the system
SQLite, which OpenClaw rejects) and the agents from npm.

## Build locally

```sh
docker build --build-arg VARIANT=kinoite -t agenticlinux:kinoite .
```

`VARIANT` is the fedora-ostree-desktops image name: `kinoite`, `silverblue`,
`sway-atomic`, `cosmic-atomic`, `xfce-atomic`, `budgie-atomic`, `base-atomic`.

## Smoke test

[test/](test) holds the VM smoke test. CI layers `test/Dockerfile` (a `test`
user and the in-guest script) on the built image, installs it into a qcow2
with `bootc install to-disk` (reading the OCI layout `docker save` produces,
writing through `qemu-nbd`), and boots it under qemu with `systemd.run=`
pointing at the script; the result is read from the serial console. To run it
locally on Linux:

```sh
docker build -f test/Dockerfile --build-arg IMAGE=agenticlinux:kinoite -t agenticlinux:smoke .
mkdir oci && docker save agenticlinux:smoke | tar x -C oci
qemu-img create -f qcow2 disk.qcow2 60G
sudo modprobe nbd max_part=16 && sudo qemu-nbd --fork -c /dev/nbd0 disk.qcow2
docker run --rm --privileged --pid=host -v /dev:/dev -v "$PWD/oci:/oci:ro" \
  -v "$PWD/usr/lib/bootc/install:/usr/lib/bootc/install:ro" quay.io/fedora/fedora-bootc:44 \
  bootc install to-disk --source-imgref oci:/oci --generic-image --skip-fetch-check \
  --karg systemd.run=/usr/bin/smoke-vm --karg systemd.run_success_action=poweroff \
  --karg systemd.run_failure_action=poweroff --karg console=ttyS0 --karg console=ttyAMA0 /dev/nbd0
sudo qemu-nbd -d /dev/nbd0
test/smoke.sh disk.qcow2
```

## CI configuration

The workflow expects the repository variable `DOCKER_HUB_USER` and secret
`DOCKER_HUB_PAT` (Docker Hub username and access token).
