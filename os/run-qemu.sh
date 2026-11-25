#!/bin/sh
# Boot the MOULI image in QEMU on the serial console.
#   os/run-qemu.sh [OUTPUT_DIR] [extra kernel args, e.g. mouli.selftest=1]
set -eu
OUT="${1:-$(dirname "$0")/../.mouli-out}"
shift || true
# The engine is built for AVX2; QEMU's default CPU model lacks it, so use "max" without KVM.
ACCEL="-cpu max"
[ -w /dev/kvm ] && ACCEL="-enable-kvm -cpu host"
exec qemu-system-x86_64 -M pc -m 2048 -smp 2 $ACCEL \
    -kernel "$OUT/images/bzImage" -initrd "$OUT/images/rootfs.cpio.gz" \
    -append "console=ttyS0 panic=-1 $*" -nographic -no-reboot
