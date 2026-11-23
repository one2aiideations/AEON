# AEON OS image

Builds a bootable AEON x86_64 system with Buildroot: a Linux 6.18 kernel, **aeond** as PID 1 and
**llama.cpp** as the local inference engine. `aeond` is the Rust crate `crates/aeond` (static musl binary,
built by `os/build.sh`); Buildroot builds the kernel, libc and llama.cpp. On boot aeond probes the hardware, picks threads and
context size, starts `llama-server` on 127.0.0.1, prints `AEON ready` and offers a chat on the
console. With `aeon.selftest=1` it sends one prompt, prints the reply and powers off; CI uses this.

```
python3 os/tools/make_tiny_gguf.py /tmp/aeon-tiny.gguf        # 0.5 MB random-weight test model
AEON_MODEL=/tmp/aeon-tiny.gguf os/build.sh                     # clones Buildroot 2026.02.3, builds (~1 h first time)
os/run-qemu.sh                                                 # boot, chat on the serial console
os/run-qemu.sh .aeon-out aeon.selftest=1                       # boot test
```

To use a real model, set `AEON_MODEL` to any GGUF file (for example a quantised 0.5–1.5B
model). RAM must hold the model; give QEMU more memory with `-m` in `os/run-qemu.sh`.

| Path | What it is |
|---|---|
| `buildroot/` | Buildroot external tree: package `aeon-llama-cpp`; config fragments; post-build installs aeond |
| `buildroot/configs/aeon_x86_64.fragment` | Options merged onto Buildroot's `qemu_x86_64_defconfig` |
| `buildroot/board/aeon/linux-aeon.fragment` | Kernel options (initramfs, devtmpfs, serial console, DRM) |
| `../crates/aeond/` | The init (Rust): mounts, console, loopback, hardware plan, server supervision, chat, self-test |
| `tools/make_tiny_gguf.py` | Writes a tiny llama-architecture GGUF for tests |

Status: unverified end to end. The Rust aeond builds as a static musl binary and its unit tests pass
on the host; nobody has booted this image yet with the Rust init (the C init was booted in CI).
Known gaps against the C init: no SIGTERM handling and no reaping of orphaned processes. The full image build and QEMU boot run in CI
(`.github/workflows/os-image.yml`). This is still the Linux-LTS kernel from Buildroot; the
aeon-kernel fork with `aeon_mem`/`aeon_gate` is PR 6. Vulkan/CUDA, model services, agents and the
chat UI follow in PRs 3–5.
