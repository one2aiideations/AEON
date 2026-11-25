# MOULI OS image

Builds a bootable MOULI x86_64 system with Buildroot: a Linux 6.18 kernel, **moulid** as PID 1 and
**llama.cpp** as the local inference engine. `moulid` is the Rust crate `crates/moulid` (static musl binary,
built by `os/build.sh`); Buildroot builds the kernel, libc and llama.cpp. On boot moulid shows the MOULI OS logo with the author's name and email (`crates/moulid/src/banner.rs`), probes the hardware, picks threads and
context size, starts `llama-server` on 127.0.0.1, prints `MOULI ready` and offers a chat on the
console. With `mouli.selftest=1` it sends one prompt, prints the reply and powers off; CI uses this.

## Put it on a USB stick and boot a PC

```
os/build.sh                      # Buildroot: Linux kernel, llama.cpp, Rust moulid (about 1 h first time)
os/make-iso.sh                   # -> .mouli-out/images/mouli.iso (BIOS and UEFI)
sudo dd if=.mouli-out/images/mouli.iso of=/dev/sdX bs=4M conv=fsync status=progress   # sdX = the USB stick!
```

CI builds `mouli.iso` on every change to `os/` or `crates/moulid` and uploads it as the
`mouli-iso` artifact (just `mouli.iso`; boot logs, screenshots and kernel parts are in `mouli-debug`), so you
can skip the local build. Write it as a raw disk image (`dd`, Rufus in DD Image mode, balenaEtcher). Check the device name with `lsblk`
before `dd`: it overwrites the target completely.

On the PC: disable **Secure Boot** (the kernel is not signed), boot from the USB stick, and pick
"MOULI" (or "safe graphics" if the screen stays blank). MOULI starts, shows the hardware it found,
and lists every `.gguf` model on any internal disk or USB stick. Type the number of a model and press
Enter; it loads and you can chat. `/models` switches model, `/clear` starts over, `/quit` powers off.

To bring a model: download any `.gguf` file (for example a 1 to 3 billion parameter model in Q4_K_M
quantisation) and copy it onto a USB stick or a disk (FAT32, exFAT, ext4, NTFS and others are read).
The stick can be the same one you booted from only if you write the files into free space on it
yourself; a second stick is simpler. Plug it in before or after boot, then press `r` Enter in the
menu. The model must fit in free RAM; the menu marks models that do not.

## Keeping settings and chat history

MOULI runs from memory, so by default nothing is saved. To keep the last-used model and your chat
logs, give it a filesystem labelled **MOULI-DATA** (ext4 or FAT, on any disk or USB stick, found
at boot without any setup). It is mounted read-write at `/mouli/data`:

- `settings`: simple `key=value` lines (the model you used last is listed first next time);
- `history/chat-0001.log`, `chat-0002.log`, ...: one file per chat, `role<TAB>text` per line;
- `models/`: put `.gguf` files here and they appear in the menu.

To make one: create an empty partition in free space on the USB stick with any partition tool,
then run `sudo os/tools/make-data-partition.sh /dev/sdX3` (it refuses whole disks and mounted
partitions and asks you to retype the device name). Or format a partition yourself with the
label `MOULI-DATA`. Chat logs are plain text on that disk: anyone with the stick can read them.
Nothing is saved to the main image, and without the partition MOULI says so at boot.

## Network

Wired Ethernet comes up by itself: MOULI waits a few seconds for a cable, asks the local network
for an address with DHCP and prints one line, for example
`MOULI network: eth0 192.168.1.20/24, dns 192.168.1.1`, or says it is offline. Intel, Realtek,
Broadcom and Atheros adapters and common USB Ethernet dongles are built in. MOULI only asks your
network for an address; it does not contact the internet by itself, and the assistant is still
told it runs offline. The system clock starts from the PC's hardware clock and is then set from three network time servers when the network is up. Wi-Fi is described below.

Self-test without a keyboard: `mouli.selftest=1` on the kernel command line loads the baked-in
model, sends one prompt and powers off. CI boots the ISO under BIOS and UEFI this way.

Development without an ISO:

```
python3 os/tools/make_tiny_gguf.py /tmp/mouli-tiny.gguf        # 0.5 MB random-weight test model
os/build.sh && MOULI_MODEL=/tmp/mouli-tiny.gguf os/make-iso.sh  # ISO with a tiny model as a second initramfs
os/run-qemu.sh .mouli-out mouli.selftest=1                       # direct-kernel boot test in QEMU
```

To use a real model, set `MOULI_MODEL` to any GGUF file (for example a quantised 0.5–1.5B
model). RAM must hold the model; give QEMU more memory with `-m` in `os/run-qemu.sh`.

| Path | What it is |
|---|---|
| `buildroot/` | Buildroot external tree: package `mouli-llama-cpp`; config fragments; post-build installs moulid |
| `buildroot/configs/mouli_x86_64.fragment` | Options merged onto Buildroot's `qemu_x86_64_defconfig` |
| `buildroot/board/mouli/linux-mouli.fragment` | Kernel options (initramfs, devtmpfs, serial console, DRM) |
| `../crates/moulid/` | The init (Rust): mounts, console, loopback, wired network, hardware plan, server supervision, chat, self-test |
| `tools/make_tiny_gguf.py` | Writes a tiny llama-architecture GGUF for tests |
| `tools/make-data-partition.sh` | Formats an existing partition as the MOULI-DATA partition |
| `release/` | Builds the release folder: ISO, installation guide PDF, `SHA256SUMS`, release notes PDF, `LICENSE.txt` (see `release/README.md`) |

Status: verified in QEMU only, not on real hardware. CI builds the image, boots the ISO under BIOS
and UEFI, loads the tiny test model, gets a chat reply and passes the self-test; the BIOS run also
attaches an `MOULI-DATA` disk and reads the saved setting and chat log back on the host. Not yet
done: anything on a physical PC, and speed or memory behaviour with a real model.

Known limits of this stage:
- Text interface on the console. There is no graphical chat window yet.
- CPU inference only, and the engine needs an AVX2 CPU (2013 or newer); moulid warns otherwise.
- Wired Ethernet and Wi-Fi (WPA/WPA2 personal and open networks), no GPU acceleration, no Secure Boot, no installer: it runs from the stick
  and keeps nothing between boots unless an `MOULI-DATA` partition is present (see above).
- Keyboards: USB, PS/2 and I2C-HID are built in; layout is US.
- moulid has no SIGTERM handling and does not reap orphaned processes.

## Hosted models (optional, online)

With a network cable connected (or after connecting Wi-Fi with `w`), press `h` in the model list to chat with a hosted model instead of a local one:
enter the provider's API address (https), the model name and your API key, and agree that messages leave the
computer. The key is typed hidden and can be saved on the MOULI-DATA partition (anyone with that stick can read
it). Emails, card numbers and passwords are replaced before sending. Local models never touch the network.

## Screen on real PCs

The serial-console boot tests cannot show a black screen. CI also boots the release ISO with a virtual
display (BIOS and UEFI) and fails when nothing is drawn (`os/tools/screencheck.py`, screenshots in the
`mouli-debug` artifact). The kernel hands the firmware framebuffer to `efifb`/`vesafb` (not `simpledrm`)
so the text console works on UEFI PCs; the build fails if the kernel configuration loses those options.
If a PC still shows nothing, pick "MOULI (verbose boot messages)" or "safe graphics" in the GRUB menu.

## Wi-Fi

Press `w` in the model list: MOULI scans, you pick a network by number, type the password (hidden) and it
connects with `wpa_supplicant` and asks for an address (DHCP). Kernel drivers and firmware for common Intel,
Realtek, MediaTek and Atheros chips are in the image (Broadcom chips need proprietary firmware and are not).
Not supported: WPA3-only and enterprise (802.1X) networks, hidden networks, remembering networks between boots.
Passwords go only to a mode-0600 file in RAM. CI tests the whole path on two virtual radios
(`mac80211_hwsim`): one is an access point, the other scans, must refuse a wrong password and join with the
right one. Real radios are not tested. The clock matters for hosted models: certificates are checked
against the date, which MOULI takes from the hardware clock and corrects from the network.
