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

Every push to `main` builds all variants, pushes them to
[Docker Hub](https://hub.docker.com/r/ericcurtin044/agenticlinux) and publishes
an installer ISO per variant and architecture on
[GitHub Releases](https://github.com/ericcurtin/agenticlinux/releases).
Images are also tagged `<variant>-<release>` matching the release tag.

## Install

Download the ISO for your variant and architecture from the latest release and
boot it. ISOs over 2 GiB are split into `.partNN` files; reassemble first:

```sh
cat agenticlinux-kinoite-x86_64.iso.part* > agenticlinux-kinoite-x86_64.iso
sha256sum -c agenticlinux-kinoite-x86_64.iso.sha256
```

Or switch an existing Fedora Atomic / bootc system:

```sh
sudo bootc switch docker.io/ericcurtin044/agenticlinux:kinoite
```

The root filesystem (which holds `/var`, `/home` and `/root`) defaults to xfs.

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
  blacklisted via kernel arguments.

Prebuilt llama.cpp and vLLM (wheels or containers) bundle their own CUDA and
ROCm user-space libraries; the host side above is what they need.

## What's inside

See [packages.txt](packages.txt) and [build.sh](build.sh). Extra
repositories used: RPM Fusion, Docker's Fedora repo, mise's rpm repo and
NVIDIA's container toolkit repo. Docker Sandboxes and llmman are installed
from their GitHub releases; the agents from npm.

## Build locally

```sh
docker build --build-arg VARIANT=kinoite -t agenticlinux:kinoite .
```

`VARIANT` is the fedora-ostree-desktops image name: `kinoite`, `silverblue`,
`sway-atomic`, `cosmic-atomic`, `xfce-atomic`, `budgie-atomic`, `base-atomic`.

## CI configuration

The workflow expects the repository variable `DOCKER_HUB_USER` and secret
`DOCKER_HUB_PAT` (Docker Hub username and access token).
