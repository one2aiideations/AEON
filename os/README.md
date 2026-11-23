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

## Keeping settings and chat history

AEON runs from memory, so by default nothing is saved. To keep the last-used model and your chat
logs, give it a filesystem labelled **AEON-DATA** (ext4 or FAT, on any disk or USB stick, found
at boot without any setup). It is mounted read-write at `/aeon/data`:

- `settings`: simple `key=value` lines (the model you used last is listed first next time);
- `history/chat-0001.log`, `chat-0002.log`, ...: one file per chat, `role<TAB>text` per line;
- `models/`: put `.gguf` files here and they appear in the menu.

To make one: create an empty partition in free space on the USB stick with any partition tool,
then run `sudo os/tools/make-data-partition.sh /dev/sdX3` (it refuses whole disks and mounted
partitions and asks you to retype the device name). Or format a partition yourself with the
label `AEON-DATA`. Chat logs are plain text on that disk: anyone with the stick can read them.
Nothing is saved to the main image, and without the partition AEON says so at boot.

## Network

Wired Ethernet comes up by itself: AEON waits a few seconds for a cable, asks the local network
for an address with DHCP and prints one line, for example
`AEON network: eth0 192.168.1.20/24, dns 192.168.1.1`, or says it is offline. Intel, Realtek,
Broadcom and Atheros adapters and common USB Ethernet dongles are built in. AEON only asks your
network for an address; it does not contact the internet by itself, and the assistant is still
told it runs offline. The system clock is set from the PC's hardware clock (no network time yet).
**Wi-Fi is not supported yet**: it needs wireless drivers, firmware files and a connect screen.

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
| `../crates/aeond/` | The init (Rust): mounts, console, loopback, wired network, hardware plan, server supervision, chat, self-test |
| `tools/make_tiny_gguf.py` | Writes a tiny llama-architecture GGUF for tests |
| `tools/make-data-partition.sh` | Formats an existing partition as the AEON-DATA partition |

Status: verified in QEMU only, not on real hardware. CI builds the image, boots the ISO under BIOS
and UEFI, loads the tiny test model, gets a chat reply and passes the self-test; the BIOS run also
attaches an `AEON-DATA` disk and reads the saved setting and chat log back on the host. Not yet
done: anything on a physical PC, and speed or memory behaviour with a real model.

Known limits of this stage:
- Text interface on the console. There is no graphical chat window yet.
- CPU inference only, and the engine needs an AVX2 CPU (2013 or newer); aeond warns otherwise.
- Wired Ethernet only (no Wi-Fi), no GPU acceleration, no Secure Boot, no installer: it runs from the stick
  and keeps nothing between boots unless an `AEON-DATA` partition is present (see above).
- Keyboards: USB, PS/2 and I2C-HID are built in; layout is US.
- aeond has no SIGTERM handling and does not reap orphaned processes.

## Hosted models (optional, online)

With a network cable connected, press `h` in the model list to chat with a hosted model instead of a local one:
enter the provider's API address (https), the model name and your API key, and agree that messages leave the
computer. The key is typed hidden and can be saved on the AEON-DATA partition (anyone with that stick can read
it). Emails, card numbers and passwords are replaced before sending. Local models never touch the network.
