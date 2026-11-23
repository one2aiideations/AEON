#!/bin/sh
# Build a bootable AEON ISO (BIOS and UEFI, usable on a USB stick) from a Buildroot output.
#   os/make-iso.sh [OUTPUT_DIR] [ISO_FILE]
# Env:
#   AEON_MODEL    optional GGUF added as a second initramfs (used by CI; real users bring their own)
#   AEON_CMDLINE  extra kernel arguments (default: none)
#   AEON_SERIAL=1 also show the boot menu and kernel console on the first serial port (for tests)
# Needs: grub-mkrescue (grub-pc-bin, grub-efi-amd64-bin), xorriso, mtools, cpio, gzip.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HERE/../.aeon-out}"
ISO="${2:-$OUT/images/aeon.iso}"
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

if [ -n "${AEON_MODEL:-}" ]; then
    [ -f "$AEON_MODEL" ] || { echo "AEON_MODEL not found: $AEON_MODEL" >&2; exit 1; }
    MD="$(mktemp -d)"
    mkdir -p "$MD/aeon/models"
    cp "$AEON_MODEL" "$MD/aeon/models/"
    (cd "$MD" && find . | cpio -o -H newc --quiet | gzip -9) > "$STAGE/boot/model.cpio.gz"
    rm -rf "$MD"
    INITRD="$INITRD /boot/model.cpio.gz"
fi

# console=tty0 last so the screen and keyboard are the main console on a PC.
CMD="quiet loglevel=3 ${AEON_CMDLINE:-}"
HEAD="set timeout=3
set default=0"
if [ "${AEON_SERIAL:-0}" = 1 ]; then
    HEAD="set timeout=1
set default=0
serial --unit=0 --speed=115200
terminal_input serial console
terminal_output serial console"
    CMD="loglevel=4 console=ttyS0,115200 ${AEON_CMDLINE:-}"
else
    CMD="$CMD console=tty0"
fi

cat > "$STAGE/boot/grub/grub.cfg" <<CFG
$HEAD
menuentry "AEON" {
    linux /boot/bzImage $CMD
    initrd $INITRD
}
menuentry "AEON (safe graphics: nomodeset)" {
    linux /boot/bzImage $CMD nomodeset
    initrd $INITRD
}
menuentry "AEON (verbose boot messages)" {
    linux /boot/bzImage loglevel=7 console=tty0 ${AEON_CMDLINE:-}
    initrd $INITRD
}
CFG

mkdir -p "$(dirname "$ISO")"
grub-mkrescue -o "$ISO" "$STAGE" -- -volid AEON >/dev/null 2>&1 || grub-mkrescue -o "$ISO" "$STAGE"
ls -la "$ISO"
echo "Write to a USB stick with:  sudo dd if=$ISO of=/dev/sdX bs=4M conv=fsync status=progress"
