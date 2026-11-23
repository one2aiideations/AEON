#!/bin/sh
# Buildroot post-build: install the Rust aeond as init and add the model, if one was provided.
#   AEOND_BIN   static aeond binary built by os/build.sh (required)
#   AEON_MODEL  optional GGUF to bake into the image
set -eu
TARGET_DIR="$1"
if [ -z "${AEOND_BIN:-}" ] || [ ! -x "${AEOND_BIN}" ]; then
    echo "AEON post-build: AEOND_BIN must point to the built aeond binary" >&2
    exit 1
fi
# The screen must work on real PCs, which QEMU's serial console never shows: fail the build when
# the kernel configuration lost an option the on-screen console needs.
KCONF="$(ls "${BUILD_DIR:-/nonexistent}"/linux-*/.config 2>/dev/null | head -1 || true)"
if [ -n "$KCONF" ]; then
    for opt in CONFIG_VT_CONSOLE CONFIG_FRAMEBUFFER_CONSOLE CONFIG_FB_EFI CONFIG_VGA_CONSOLE CONFIG_EFI_STUB; do
        grep -q "^$opt=y" "$KCONF" || { echo "AEON post-build: kernel config lacks $opt" >&2; exit 1; }
    done
    if grep -q "^CONFIG_SYSFB_SIMPLEFB=y" "$KCONF"; then
        echo "AEON post-build: SYSFB_SIMPLEFB must be off (black screen on UEFI)" >&2
        exit 1
    fi
    echo "AEON post-build: kernel screen options verified"
fi
install -D -m 0755 "${AEOND_BIN}" "${TARGET_DIR}/sbin/aeond"
ln -sf sbin/aeond "${TARGET_DIR}/init"          # initramfs: the kernel runs /init as PID 1
ln -sf aeond "${TARGET_DIR}/sbin/init"
mkdir -p "${TARGET_DIR}/aeon/models" "${TARGET_DIR}/var/log"
if [ -n "${AEON_MODEL:-}" ] && [ -f "${AEON_MODEL}" ]; then
    cp "${AEON_MODEL}" "${TARGET_DIR}/aeon/models/"
    echo "AEON post-build: added model $(basename "${AEON_MODEL}")"
else
    echo "AEON post-build: no AEON_MODEL given; image boots without a model"
fi
