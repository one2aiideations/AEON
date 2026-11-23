# AEON OS image

Builds a bootable AEON x86_64 system with Buildroot: a Linux 6.18 kernel, **aeond** as PID 1 and
**llama.cpp** as the local inference engine. `aeond` is the Rust crate `crates/aeond` (static musl binary,
built by `os/build.sh`); Buildroot builds the kernel, libc and llama.cpp. On boot aeond probes the hardware, picks threads and
context size, starts `llama-server` on 127.0.0.1, prints `AEON ready` and offers a chat on the
console. With `aeon.selftest=1` it sends one prompt, prints the reply and powers off; CI uses this.

## Put it on a USB stick and boot a PC

```
os/build.sh                      # Buildroot: Linux kernel, llama.cpp, Rust aeond (about 1 h first time)
os/make-iso.sh                   # -> .aeon-out/images/aeon.iso (BIOS and UEFI)
sudo dd if=.aeon-out/images/aeon.iso of=/dev/sdX bs=4M conv=fsync status=progress   # sdX = the USB stick!
```

CI builds `aeon.iso` on every change to `os/` or `crates/aeond` and uploads it as the
`aeon-x86_64-image` artifact, so you can skip the local build. Check the device name with `lsblk`
before `dd`: it overwrites the target completely.

On the PC: disable **Secure Boot** (the kernel is not signed), boot from the USB stick, and pick
"AEON" (or "safe graphics" if the screen stays blank). AEON starts, shows the hardware it found,
and lists every `.gguf` model on any internal disk or USB stick. Type the number of a model and press
Enter; it loads and you can chat. `/models` switches model, `/clear` starts over, `/quit` powers off.

To bring a model: download any `.gguf` file (for example a 1 to 3 billion parameter model in Q4_K_M
quantisation) and copy it onto a USB stick or a disk (FAT32, exFAT, ext4, NTFS and others are read).
The stick can be the same one you booted from only if you write the files into free space on it
yourself; a second stick is simpler. Plug it in before or after boot, then press `r` Enter in the
menu. The model must fit in free RAM; the menu marks models that do not.

Self-test without a keyboard: `aeon.selftest=1` on the kernel command line loads the baked-in
model, sends one prompt and powers off. CI boots the ISO under BIOS and UEFI this way.

Development without an ISO:

```
python3 os/tools/make_tiny_gguf.py /tmp/aeon-tiny.gguf        # 0.5 MB random-weight test model
os/build.sh && AEON_MODEL=/tmp/aeon-tiny.gguf os/make-iso.sh  # ISO with a tiny model as a second initramfs
os/run-qemu.sh .aeon-out aeon.selftest=1                       # direct-kernel boot test in QEMU
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

Status: unverified on real hardware. What has run: the Rust `aeond` (picker, chat, self-test) end to
end on a development machine against a stand-in inference server, plus its unit tests. What has
not: building the image with this kernel configuration, booting the ISO under BIOS or UEFI (CI does
this on the first run), anything on a physical PC, and speed or memory behaviour with a real model.

Known limits of this stage:
- Text interface on the console. There is no graphical chat window yet.
- CPU inference only, and the engine needs an AVX2 CPU (2013 or newer); aeond warns otherwise.
- No Wi-Fi or network, no GPU acceleration, no Secure Boot, no installer: it runs from the stick
  and keeps nothing between boots (chat history is not saved).
- Keyboards: USB, PS/2 and I2C-HID are built in; layout is US.
- aeond has no SIGTERM handling and does not reap orphaned processes.
