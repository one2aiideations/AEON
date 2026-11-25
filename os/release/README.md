# Release folder

Every product release is one folder with the same five parts:

```
MOULI-OS-Release-v1.0/
  01-ISO/MOULI-OS.iso
  02-Installation-Guide/Installation-Guide.pdf
  03-Verification/SHA256SUMS
  04-Release-Notes/Release-Notes.pdf
  05-Licensing/LICENSE.txt
```

```
os/release/make-release.sh ISO OUTPUT_DIR [VERSION]      # VERSION defaults to 1.0
PRODUCT_NAME="MOULI Mail" PRODUCT_FILE=MOULI-Mail os/release/make-release.sh mail.iso out 1.0
```

The script copies the ISO, fills the version, date, product name and the ISO's SHA-256 into the
three text sources in this folder, turns the guide and the notes into PDFs, writes `SHA256SUMS`
(every other file, paths from the release folder) and checks it. It refuses an existing folder
unless `FORCE=1`, a bad version or name, and any unfilled `@PLACEHOLDER@`.

To check a release, from the release folder: `sha256sum -c 03-Verification/SHA256SUMS`.

| File | Purpose |
|---|---|
| `INSTALLATION-GUIDE.txt`, `RELEASE-NOTES.txt`, `LICENSE.txt` | The text of the three documents. Edit these, not the PDFs. |
| `textpdf.py` | Plain text to PDF with the standard library only (no installs in CI). Format: `#` and `##` headings, `- ` bullets, 4-space indent for code. ASCII only. |
| `test_release.py` | `python3 -m unittest discover -s os/release` |

CI builds the folder from the release ISO and uploads it as the `MOULI-OS-Release-v<version>`
artifact. The version is `MOULI_RELEASE_VERSION` in `.github/workflows/os-image.yml`.

Before a release is distributed, the author must confirm section 1 of `LICENSE.txt`: no licence
for MOULI's own code has been chosen yet (the workspace says `LicenseRef-MOULI-undecided`).
