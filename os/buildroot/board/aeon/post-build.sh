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
# Wi-Fi tools, regulatory database and firmware must have been installed into the image.
for f in usr/sbin/wpa_supplicant usr/sbin/wpa_cli usr/sbin/iw; do
    [ -x "${TARGET_DIR}/$f" ] || [ -x "${TARGET_DIR}/${f#usr/}" ] || { echo "AEON post-build: $f missing from the image" >&2; exit 1; }
done
ls "${TARGET_DIR}"/lib/firmware/regulatory.db >/dev/null 2>&1 || { echo "AEON post-build: regulatory.db missing" >&2; exit 1; }
ls "${TARGET_DIR}"/lib/firmware/iwlwifi-* >/dev/null 2>&1 || { echo "AEON post-build: no Intel Wi-Fi firmware in the image" >&2; exit 1; }
# The screen must work on real PCs, which QEMU's serial console never shows: fail the build when
# the kernel configuration lost an option the on-screen console needs.
KCONF="$(ls "${BUILD_DIR:-/nonexistent}"/linux-*/.config 2>/dev/null | head -1 || true)"
if [ -n "$KCONF" ]; then
    for opt in CONFIG_VT_CONSOLE CONFIG_FRAMEBUFFER_CONSOLE CONFIG_FB_EFI CONFIG_VGA_CONSOLE CONFIG_EFI_STUB CONFIG_CFG80211 CONFIG_MAC80211 CONFIG_IWLWIFI CONFIG_MAC80211_HWSIM; do
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

# Networking needs BusyBox's udhcpc, its configuration script and `ip`; fail the build now rather
# than boot an image that silently has no network.
for need in usr/share/udhcpc/default.script bin/busybox; do
    [ -e "${TARGET_DIR}/${need}" ] || { echo "AEON post-build: ${need} missing" >&2; exit 1; }
done
for applet in udhcpc ip hwclock; do
    "${TARGET_DIR}/bin/busybox" 2>&1 | tr ',' '\n' | grep -qw "$applet" \
        || strings "${TARGET_DIR}/bin/busybox" 2>/dev/null | grep -qx "$applet" \
        || { echo "AEON post-build: BusyBox lacks the $applet applet" >&2; exit 1; }
done
echo "AEON post-build: networking tools present"
