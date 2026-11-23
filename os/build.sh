#!/bin/sh
# Build the AEON x86_64 image with Buildroot.
#   os/build.sh [BUILDROOT_DIR] [OUTPUT_DIR]
# Env: AEON_MODEL=/path/model.gguf to bake a model into the image.
# aeond (the Rust init, crates/aeond) is built here as a static musl binary and installed by the
# Buildroot post-build script; Buildroot itself only builds the kernel, libc and llama.cpp.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
BR="${1:-$HERE/../.buildroot}"
OUT="${2:-$HERE/../.aeon-out}"
BR_VERSION="${BR_VERSION:-2026.02.3}"
ROOT="$(cd "$HERE/.." && pwd)"
TARGET="x86_64-unknown-linux-musl"
cargo build --release --locked -p aeond --target "$TARGET" --manifest-path "$ROOT/Cargo.toml"
AEOND_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/$TARGET/release/aeond"
[ -x "$AEOND_BIN" ] || { echo "aeond build failed: $AEOND_BIN missing"; exit 1; }
export AEOND_BIN
if [ ! -d "$BR" ]; then
    git clone --depth 1 --branch "$BR_VERSION" https://github.com/buildroot/buildroot "$BR"
fi
make -C "$BR" BR2_EXTERNAL="$HERE/buildroot" O="$OUT" qemu_x86_64_defconfig
cat "$HERE/buildroot/configs/aeon_x86_64.fragment" >> "$OUT/.config"
make -C "$BR" O="$OUT" olddefconfig
for opt in BR2_PACKAGE_AEON_LLAMA_CPP BR2_TARGET_ROOTFS_CPIO BR2_INIT_NONE BR2_LINUX_KERNEL_USE_DEFCONFIG BR2_PACKAGE_CA_CERTIFICATES; do
    grep -q "^$opt=y" "$OUT/.config" || { echo "config check failed: $opt"; exit 1; }
done
make -C "$BR" O="$OUT" -j"$(nproc)"
ls -la "$OUT/images"
