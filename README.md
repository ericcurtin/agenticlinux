<p align="center">
  <img src="https://github.com/ericcurtin/agenticlinux/releases/download/assets/agenticlinux-logo-256.png" alt="AgenticLinux logo" width="160">
</p>

<h1 align="center">AgenticLinux</h1>

A [bootc](https://bootc-dev.github.io/bootc/) desktop for working with coding
agents: [Docker Engine](https://docs.docker.com/engine/),
[Docker Sandboxes](https://docs.docker.com/ai/sandboxes/),
[llmman](https://github.com/llmmanorg/llmman), the `claude`, `codex`,
`opencode` and `openclaw` agents, GPU runtimes (Vulkan, ROCm, NVIDIA/CUDA) and
a developer toolset preinstalled. Built from Fedora 44's packages on the
[fedora-ostree-desktops](https://quay.io/organization/fedora-ostree-desktops)
images. x86_64 and aarch64.

| Variant | Desktop    | Image                                         |
|---------|------------|-----------------------------------------------|
| kde     | KDE Plasma | `docker.io/ericcurtin044/agenticlinux:kde`    |
| gnome   | GNOME      | `docker.io/ericcurtin044/agenticlinux:gnome`  |
| sway    | Sway       | `docker.io/ericcurtin044/agenticlinux:sway`   |
| cosmic  | COSMIC     | `docker.io/ericcurtin044/agenticlinux:cosmic` |
| xfce    | Xfce       | `docker.io/ericcurtin044/agenticlinux:xfce`   |
| budgie  | Budgie     | `docker.io/ericcurtin044/agenticlinux:budgie` |
| base    | none       | `docker.io/ericcurtin044/agenticlinux:base`   |

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
boot it. It is a network installer preset to pull the matching image from
Docker Hub, so the install needs a network connection; disk, user and locale are
chosen in the installer as usual, with plain xfs partitions as the default.

Or switch an existing bootc system:

```sh
sudo bootc switch docker.io/ericcurtin044/agenticlinux:kde
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
