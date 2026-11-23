#!/bin/sh
# Boot the AEON image in QEMU on the serial console.
#   os/run-qemu.sh [OUTPUT_DIR] [extra kernel args, e.g. aeon.selftest=1]
set -eu
OUT="${1:-$(dirname "$0")/../.aeon-out}"
shift || true
ACCEL=""
[ -w /dev/kvm ] && ACCEL="-enable-kvm -cpu host"
exec qemu-system-x86_64 -M pc -m 2048 -smp 2 $ACCEL \
    -kernel "$OUT/images/bzImage" -initrd "$OUT/images/rootfs.cpio.gz" \
    -append "console=ttyS0 panic=-1 $*" -nographic -no-reboot
