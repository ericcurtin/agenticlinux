<p align="center">
  <img src="https://github.com/ericcurtin/agenticlinux/releases/download/assets/agenticlinux-logo-256.png" alt="AgenticLinux logo" width="160">
</p>

<h1 align="center">AgenticLinux</h1>

A Linux desktop built for working with AI agents. Everything is there on
first boot: the `claude`, `codex`, `opencode` and `openclaw` agents,
[llmman](https://github.com/llmmanorg/llmman) to run models locally or connect
any agent to any provider, [Docker Engine](https://docs.docker.com/engine/) and
[Docker Sandboxes](https://docs.docker.com/ai/sandboxes/) to run agents in
isolation, GPU runtimes for Vulkan, ROCm and NVIDIA/CUDA, and a full developer
toolset.

AgenticLinux is a [bootc](https://bootc-dev.github.io/bootc/) image: the whole
OS ships as a container, so updates are atomic and rollback is one command.
It is built from Fedora 44 packages on the
[fedora-ostree-desktops](https://quay.io/organization/fedora-ostree-desktops)
images and available for x86_64 and aarch64.

<p align="center">
  <img src="https://github.com/ericcurtin/agenticlinux/releases/download/assets/agenticlinux-screenshot-kde.png" alt="AgenticLinux KDE Plasma desktop with the agents and tools listed in a terminal" width="900">
</p>

## Quick start

1. Download the ISO for your desktop and architecture from the
   [latest release](https://github.com/ericcurtin/agenticlinux/releases/latest),
   boot it and install as usual.
2. Add yourself to the `docker` and `kvm` groups, then log out and back in:

   ```sh
   sudo usermod -aG docker,kvm "$USER"
   ```

3. Run an agent on a local model, or on any hosted provider:

   ```sh
   llmman launch claude --model qwen3.8
   llmman launch opencode --provider openrouter --model qwen/qwen3-coder
   ```

| Variant | Desktop    | Image                                         |
|---------|------------|-----------------------------------------------|
| kde     | KDE Plasma | `docker.io/ericcurtin044/agenticlinux:kde`    |
| gnome   | GNOME      | `docker.io/ericcurtin044/agenticlinux:gnome`  |
| sway    | Sway       | `docker.io/ericcurtin044/agenticlinux:sway`   |
| cosmic  | COSMIC     | `docker.io/ericcurtin044/agenticlinux:cosmic` |
| xfce    | Xfce       | `docker.io/ericcurtin044/agenticlinux:xfce`   |
| budgie  | Budgie     | `docker.io/ericcurtin044/agenticlinux:budgie` |
| base    | none       | `docker.io/ericcurtin044/agenticlinux:base`   |

## Install

The ISO is a network installer preset to pull the matching image from Docker
Hub, so the install needs a network connection; disk, user and locale are
chosen in the installer as usual, with plain xfs partitions as the default.

Or switch an existing bootc system:

```sh
sudo bootc switch docker.io/ericcurtin044/agenticlinux:kde
```

The root filesystem (which holds `/var`, `/home` and `/root`) defaults to xfs
for both `bootc install` and the ISO. The `kvm` group from the quick start is
what lets Docker Sandboxes open `/dev/kvm`.

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
