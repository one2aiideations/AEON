#!/bin/sh
# Build a bootable MOULI ISO (BIOS and UEFI, usable on a USB stick) from a Buildroot output.
#   os/make-iso.sh [OUTPUT_DIR] [ISO_FILE]
# Env:
#   MOULI_MODEL    optional GGUF added as a second initramfs (used by CI; real users bring their own)
#   MOULI_CMDLINE  extra kernel arguments (default: none)
#   MOULI_SERIAL=1 also show the boot menu and kernel console on the first serial port (for tests)
# Needs: grub-mkrescue (grub-pc-bin, grub-efi-amd64-bin), xorriso, mtools, cpio, gzip.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HERE/../.mouli-out}"
ISO="${2:-$OUT/images/mouli.iso}"
KERNEL="$OUT/images/bzImage"
ROOTFS="$OUT/images/rootfs.cpio.gz"
for f in "$KERNEL" "$ROOTFS"; do [ -f "$f" ] || { echo "missing $f (run os/build.sh first)" >&2; exit 1; }; done
command -v grub-mkrescue >/dev/null || { echo "grub-mkrescue not found (install grub-pc-bin grub-efi-amd64-bin xorriso mtools)" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/boot/grub"
cp "$KERNEL" "$STAGE/boot/bzImage"
cp "$ROOTFS" "$STAGE/boot/rootfs.cpio.gz"
INITRD="/boot/rootfs.cpio.gz"

if [ -n "${MOULI_MODEL:-}" ]; then
    [ -f "$MOULI_MODEL" ] || { echo "MOULI_MODEL not found: $MOULI_MODEL" >&2; exit 1; }
    MD="$(mktemp -d)"
    mkdir -p "$MD/mouli/models"
    cp "$MOULI_MODEL" "$MD/mouli/models/"
    (cd "$MD" && find . | cpio -o -H newc --quiet | gzip -9) > "$STAGE/boot/model.cpio.gz"
    rm -rf "$MD"
    INITRD="$INITRD /boot/model.cpio.gz"
fi

# console=tty0 last so the screen and keyboard are the main console on a PC.
CMD="quiet loglevel=3 ${MOULI_CMDLINE:-}"
HEAD="set timeout=3
set default=0"
if [ "${MOULI_SERIAL:-0}" = 1 ]; then
    HEAD="set timeout=1
set default=0
serial --unit=0 --speed=115200
terminal_input serial console
terminal_output serial console"
    CMD="loglevel=4 console=ttyS0,115200 ${MOULI_CMDLINE:-}"
else
    CMD="$CMD console=tty0"
fi

cat > "$STAGE/boot/grub/grub.cfg" <<CFG
$HEAD
menuentry "MOULI" {
    linux /boot/bzImage $CMD
    initrd $INITRD
}
menuentry "MOULI (safe graphics: nomodeset)" {
    linux /boot/bzImage $CMD nomodeset
    initrd $INITRD
}
menuentry "MOULI (verbose boot messages)" {
    linux /boot/bzImage loglevel=7 console=tty0 ${MOULI_CMDLINE:-}
    initrd $INITRD
}
CFG

mkdir -p "$(dirname "$ISO")"
grub-mkrescue -o "$ISO" "$STAGE" -- -volid MOULI >/dev/null 2>&1 || grub-mkrescue -o "$ISO" "$STAGE"
# grub-mkrescue silently makes a UEFI-only ISO when grub-pc-bin is missing; refuse that.
xorriso -indev "$ISO" -report_el_torito plain 2>/dev/null | grep -q "BIOS" || {
    echo "ISO has no BIOS boot image: install grub-pc-bin and run again" >&2
    exit 1
}
ls -la "$ISO"
echo "Write to a USB stick with:  sudo dd if=$ISO of=/dev/sdX bs=4M conv=fsync status=progress"
