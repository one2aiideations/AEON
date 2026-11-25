#!/bin/sh
# Assemble a product release folder:
#
#   <PRODUCT_FILE>-Release-v<VERSION>/
#     01-ISO/<PRODUCT_FILE>.iso
#     02-Installation-Guide/Installation-Guide.pdf
#     03-Verification/SHA256SUMS
#     04-Release-Notes/Release-Notes.pdf
#     05-Licensing/LICENSE.txt
#
#   os/release/make-release.sh ISO OUTPUT_DIR [VERSION]
#
# VERSION defaults to 1.0. Environment:
#   PRODUCT_NAME   shown in the documents      (default "MOULI OS")
#   PRODUCT_FILE   used in file and folder names (default "MOULI-OS")
#   RELEASE_DATE   YYYY-MM-DD in the documents (default: today, UTC)
#   FORCE=1        replace an existing release folder
#
# SHA256SUMS lists every other file with its path from the release folder, so from that folder
# `sha256sum -c 03-Verification/SHA256SUMS` checks the whole release. Needs python3 and sha256sum.
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
ISO="${1:-}"
OUT="${2:-}"
VERSION="${3:-1.0}"
PRODUCT_NAME="${PRODUCT_NAME:-MOULI OS}"
PRODUCT_FILE="${PRODUCT_FILE:-MOULI-OS}"
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%Y-%m-%d)}"

die() { echo "make-release: $*" >&2; exit 1; }
[ -n "$ISO" ] && [ -n "$OUT" ] || die "usage: make-release.sh ISO OUTPUT_DIR [VERSION]"
[ -f "$ISO" ] && [ -s "$ISO" ] || die "ISO not found or empty: $ISO"
case "$VERSION" in
    ''|*[!0-9.]*|.*|*.|*..*) die "VERSION must look like 1.0 or 1.2.3, not '$VERSION'" ;;
esac
case "$PRODUCT_FILE" in
    ''|*[!A-Za-z0-9._-]*) die "PRODUCT_FILE may only contain letters, digits, . _ -" ;;
esac
case "$RELEASE_DATE" in
    [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
    *) die "RELEASE_DATE must be YYYY-MM-DD" ;;
esac
for f in INSTALLATION-GUIDE.txt RELEASE-NOTES.txt LICENSE.txt textpdf.py; do
    [ -f "$HERE/$f" ] || die "missing $HERE/$f"
done
command -v python3 >/dev/null || die "python3 not found"
command -v sha256sum >/dev/null || die "sha256sum not found"

FOLDER="${PRODUCT_FILE}-Release-v${VERSION}"
DIR="$OUT/$FOLDER"
if [ -e "$DIR" ]; then
    [ "${FORCE:-0}" = 1 ] || die "$DIR exists (set FORCE=1 to replace it)"
    rm -rf "$DIR"
fi
mkdir -p "$DIR/01-ISO" "$DIR/02-Installation-Guide" "$DIR/03-Verification" "$DIR/04-Release-Notes" "$DIR/05-Licensing"

cp "$ISO" "$DIR/01-ISO/$PRODUCT_FILE.iso"
ISO_SHA256="$(sha256sum "$DIR/01-ISO/$PRODUCT_FILE.iso" | cut -d' ' -f1)"
ISO_BYTES="$(wc -c < "$DIR/01-ISO/$PRODUCT_FILE.iso")"
ISO_SIZE_MB=$(( (ISO_BYTES + 1048575) / 1048576 ))

# Fill in the names, version, date and ISO checksum; stop if any @TOKEN@ is left over.
fill() {
    python3 - "$1" "$2" "$PRODUCT_NAME" "$PRODUCT_FILE" "$VERSION" "$RELEASE_DATE" "$ISO_SHA256" "$ISO_SIZE_MB" "$FOLDER" <<'PY'
import re, sys
src, dst, name, file, version, date, sha, mb, folder = sys.argv[1:]
text = open(src, encoding="ascii").read()
for k, v in {"PRODUCT_NAME": name, "PRODUCT_FILE": file, "VERSION": version, "DATE": date,
             "ISO_SHA256": sha, "ISO_SIZE_MB": mb, "PRODUCT_FOLDER": folder}.items():
    text = text.replace(f"@{k}@", v)
left = re.findall(r"@[A-Z0-9_]+@", text)
if left:
    sys.exit(f"unfilled placeholders in {src}: {sorted(set(left))}")
open(dst, "w", encoding="ascii", newline="\n").write(text)
PY
}
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fill "$HERE/INSTALLATION-GUIDE.txt" "$TMP/guide.txt"
fill "$HERE/RELEASE-NOTES.txt" "$TMP/notes.txt"
fill "$HERE/LICENSE.txt" "$DIR/05-Licensing/LICENSE.txt"

AUTHOR="Chandra Mouli Krishna Moorthy"
FOOTER="$PRODUCT_NAME $VERSION"
python3 "$HERE/textpdf.py" "$TMP/guide.txt" "$DIR/02-Installation-Guide/Installation-Guide.pdf" \
    --title "$PRODUCT_NAME $VERSION - Installation Guide" --author "$AUTHOR" --footer "$FOOTER - Installation Guide"
python3 "$HERE/textpdf.py" "$TMP/notes.txt" "$DIR/04-Release-Notes/Release-Notes.pdf" \
    --title "$PRODUCT_NAME $VERSION - Release Notes" --author "$AUTHOR" --footer "$FOOTER - Release Notes"

# Checksums of every file except the list itself, paths from the release folder, sorted.
(
    cd "$DIR"
    find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | while IFS= read -r f; do
        sha256sum "$f"
    done > 03-Verification/SHA256SUMS
    sha256sum -c 03-Verification/SHA256SUMS >/dev/null
)

# The five parts must all be there and not empty.
for f in "01-ISO/$PRODUCT_FILE.iso" 02-Installation-Guide/Installation-Guide.pdf 03-Verification/SHA256SUMS \
         04-Release-Notes/Release-Notes.pdf 05-Licensing/LICENSE.txt; do
    [ -s "$DIR/$f" ] || die "missing or empty: $f"
done
echo "Release folder: $DIR"
(cd "$OUT" && find "$FOLDER" -type f | LC_ALL=C sort | while IFS= read -r f; do printf '  %s (%s bytes)\n' "$f" "$(wc -c < "$f")"; done)
echo "ISO SHA-256: $ISO_SHA256"
