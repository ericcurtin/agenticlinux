<p align="center">
  <img src="https://github.com/ericcurtin/agenticlinux/releases/download/assets/agenticlinux-logo-256.png" alt="AgenticLinux logo" width="160">
</p>

<h1 align="center">AgenticLinux</h1>

A Linux desktop built for working with AI agents. Everything is there on
first boot: the `claude`, `codex`, `opencode` and `openclaw` agents,
[herdr](https://herdr.dev) to run them side by side, the
[ChatGPT](https://learn.chatgpt.com/docs/app) (with Codex) and
[OpenCode](https://opencode.ai) desktop apps, Google Chrome,
[llmman](https://github.com/llmmanorg/llmman) to run models locally or connect
any agent to any provider, [Docker Engine](https://docs.docker.com/engine/) and
[Docker Sandboxes](https://docs.docker.com/ai/sandboxes/) to run agents in
isolation, GPU runtimes for Vulkan, ROCm and NVIDIA/CUDA, and a full developer
toolset.

AgenticLinux is a [bootc](https://bootc-dev.github.io/bootc/) image: the whole
OS ships as a container, so updates are atomic and rollback is one command.
It is built from Fedora 44 packages on the
[fedora-ostree-desktops](https://quay.io/organization/fedora-ostree-desktops)
images, or from CentOS Stream 10 packages (with EPEL) on the
[centos-bootc](https://quay.io/repository/centos-bootc/centos-bootc) image,
and available for x86_64 and aarch64.

<p align="center">
  <a href="https://github.com/ericcurtin/ericcurtin.github.io/releases/download/assets/openclaw-web.mp4">
    <img src="https://github.com/ericcurtin/ericcurtin.github.io/releases/download/assets/openclaw-web.webp" alt="OpenClaw's web Control UI on the AgenticLinux KDE desktop, asked to check the machine's health, turn on automatic OS updates and reclaim Docker disk (click for the video)" width="900">
  </a>
</p>

## Quick start

1. Download the ISO for your desktop and architecture from the
   [latest release](https://github.com/ericcurtin/agenticlinux/releases/latest),
   boot it and install as usual.
2. Add yourself to the `docker` group, then log out and back in:

   ```sh
   sudo usermod -aG docker "$USER"
   ```

3. Run an agent on a local model, or on any hosted provider:

   ```sh
   llmman launch claude --model qwen3.8
   llmman launch opencode --provider openrouter --model qwen/qwen3-coder
   ```

| Variant      | Built from       | Desktop    | Image                                               |
|--------------|------------------|------------|-----------------------------------------------------|
| kde          | Fedora 44        | KDE Plasma | `docker.io/ericcurtin044/agenticlinux:kde`          |
| gnome        | Fedora 44        | GNOME      | `docker.io/ericcurtin044/agenticlinux:gnome`        |
| sway         | Fedora 44        | Sway       | `docker.io/ericcurtin044/agenticlinux:sway`         |
| cosmic       | Fedora 44        | COSMIC     | `docker.io/ericcurtin044/agenticlinux:cosmic`       |
| xfce         | Fedora 44        | Xfce       | `docker.io/ericcurtin044/agenticlinux:xfce`         |
| budgie       | Fedora 44        | Budgie     | `docker.io/ericcurtin044/agenticlinux:budgie`       |
| base         | Fedora 44        | none       | `docker.io/ericcurtin044/agenticlinux:base`         |
| centos-kde   | CentOS Stream 10 | KDE Plasma | `docker.io/ericcurtin044/agenticlinux:centos-kde`   |
| centos-gnome | CentOS Stream 10 | GNOME      | `docker.io/ericcurtin044/agenticlinux:centos-gnome` |
| centos-base  | CentOS Stream 10 | none       | `docker.io/ericcurtin044/agenticlinux:centos-base`  |

The variants carry the same packages on both distributions, so the list is
what CentOS Stream 10, EPEL 10 and RPM Fusion have: GNOME and KDE Plasma are
the desktops that exist there. CentOS Stream's kernel tracks the next RHEL
10 minor release.

## Desktop apps

The desktop apps of the agents that publish an RPM are installed from it,
and start from the applications menu or as `chatgpt` and `opencode-desktop`:

- **ChatGPT**: OpenAI's one desktop app, which is also the Codex app (the
  "Codex" mode inside it). Its Linux build is a preview.
- **OpenCode**: relocated from `/opt` to `/usr/lib` so it is part of the
  image rather than of the machine's first install.

The apps live in the read-only `/usr`, so nothing in them can update itself
in place: they are updated with the image, like everything else.

## Install

The ISO is a network installer preset to pull the matching image from Docker
Hub, so the install needs a network connection; disk, user and locale are
chosen in the installer as usual, with plain xfs partitions as the default.

Or switch an existing bootc system:

```sh
sudo bootc switch docker.io/ericcurtin044/agenticlinux:kde
```

The root filesystem (which holds `/var`, `/home` and `/root`) defaults to xfs
for both `bootc install` and the ISO. `/dev/kvm`, which Docker Sandboxes
need, is open to every user: Fedora and CentOS Stream make it mode 0666, so
no `kvm` group membership is involved.

## GPUs

- Vulkan: Mesa drivers and `vulkaninfo`.
- ROCm (x86_64): HIP runtime, OpenCL, rocBLAS, hipBLAS, hipBLASLt, RCCL,
  `rocminfo`, `rocm-smi`. Containers get GPU access with
  `--device /dev/kfd --device /dev/dri`.
- ROCm on CentOS Stream is EPEL's build: HIP, rocBLAS, hipBLAS, hipBLASLt,
  RCCL, `rocminfo`, `rocm-smi`; EPEL has no ROCm OpenCL.
- NVIDIA: the RPM Fusion driver with the kernel module prebuilt for the
  image's kernel, CUDA driver libraries and `nvidia-container-toolkit`
  registered with Docker (`docker run --gpus all ...`). The module is
  unsigned, so disable Secure Boot or enroll your own MOK. nouveau is
  blacklisted via kernel arguments. On CentOS Stream the driver is RPM
  Fusion's EL10 build, the 580 series. On aarch64 the driver is best effort:
  when RPM Fusion's aarch64 build is broken the image is published without
  it (and with nouveau), see [build.sh](build.sh).
