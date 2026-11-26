#!/usr/bin/env bash
# Best-effort LibreOffice interoperability smoke test.
# Usage: tools/interop.sh [project-directory] [mouli-studio-binary]
set -u
PROJECT_DIR="${1:-.}"
BIN="${2:-mouli-studio}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PASS=0
FAIL=0
SKIP=0
report() {
  if [ "$2" = pass ]; then PASS=$((PASS+1)); printf 'PASS: %s\n' "$1"
  elif [ "$2" = fail ]; then FAIL=$((FAIL+1)); printf 'FAIL: %s\n' "$1"
  else SKIP=$((SKIP+1)); printf 'SKIP: %s\n' "$1"; fi
}
if ! command -v libreoffice >/dev/null 2>&1; then
  echo "LibreOffice not installed; interoperability checks not run."
  exit 0
fi
if ! command -v "$BIN" >/dev/null 2>&1 && [ ! -x "$BIN" ]; then
  echo "mouli-studio executable not found ($BIN); cannot generate fixtures."
  exit 2
fi
mkdir -p "$TMP/project"
(
  cd "$TMP/project" || exit 2
  "$BIN" new sample.md "Interop Smoke Test" >/dev/null 2>&1
  "$BIN" convert sample.md --to docx --out sample.docx >/dev/null 2>&1
)
if [ -s "$TMP/project/sample.docx" ]; then
  report "MOULI CLI generated DOCX fixture" pass
  mkdir -p "$TMP/out"
  if libreoffice --headless --convert-to pdf --outdir "$TMP/out" "$TMP/project/sample.docx" >/dev/null 2>&1 \
      && [ -s "$TMP/out/sample.pdf" ]; then
    report "LibreOffice opened exported DOCX and produced PDF" pass
  else
    report "LibreOffice DOCX conversion" fail
  fi
else
  report "MOULI CLI DOCX fixture generation" fail
fi
# Additional samples may be placed in the project directory by CI.
for ext in xlsx pptx; do
  found=0
  while IFS= read -r -d '' file; do
    found=1
    mkdir -p "$TMP/out-$ext"
    if libreoffice --headless --convert-to pdf --outdir "$TMP/out-$ext" "$file" >/dev/null 2>&1; then
      report "LibreOffice opened $(basename "$file")" pass
    else
      report "LibreOffice open $(basename "$file")" fail
    fi
  done < <(find "$PROJECT_DIR" -type f -name "*.$ext" -print0 2>/dev/null)
  if [ "$found" -eq 0 ]; then report "No $ext interoperability fixture supplied" skip; fi
done
printf 'Interop summary: %s passed, %s failed, %s skipped\n' "$PASS" "$FAIL" "$SKIP"
[ "$FAIL" -eq 0 ]
