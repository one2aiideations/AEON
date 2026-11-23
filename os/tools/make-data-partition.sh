#!/bin/sh
# Format ONE EXISTING PARTITION as AEON's data partition (ext4, label AEON-DATA).
#
#   sudo os/tools/make-data-partition.sh /dev/sdb3
#
# Everything on that partition is erased. Create the (empty) partition first with any partition
# tool (GParted, fdisk, Disks), for example in the free space after the AEON image on your USB
# stick. This script refuses whole disks, mounted partitions and the partition holding / .
set -eu

dev="${1:-}"
[ -n "$dev" ] || { echo "usage: $0 /dev/PARTITION" >&2; exit 2; }
[ "$(id -u)" -eq 0 ] || { echo "run as root (sudo)" >&2; exit 1; }
[ -b "$dev" ] || { echo "$dev is not a block device" >&2; exit 1; }

# Refuse a whole disk: only partitions (they have a parent disk).
if [ "$(lsblk -no TYPE "$dev" 2>/dev/null | head -1)" != "part" ]; then
  echo "$dev is not a partition; create a partition first" >&2
  exit 1
fi
# Refuse anything mounted, and the device that holds the running system.
if grep -q "^$dev " /proc/mounts; then
  echo "$dev is mounted; unmount it first" >&2
  exit 1
fi
root_dev="$(findmnt -no SOURCE / 2>/dev/null || true)"
[ "$root_dev" != "$dev" ] || { echo "$dev holds the running system" >&2; exit 1; }

echo "About to ERASE $dev and format it as ext4 with label AEON-DATA:"
lsblk -o NAME,SIZE,FSTYPE,LABEL,MOUNTPOINT "$dev"
printf 'Type the device name again to confirm (%s): ' "$dev"
read -r answer
[ "$answer" = "$dev" ] || { echo "not confirmed; nothing changed"; exit 1; }

mkfs.ext4 -F -L AEON-DATA "$dev"
echo "Done. AEON will save settings and chat history there, and also loads models from its models/ folder."
