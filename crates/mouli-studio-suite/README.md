# mouli-studio-suite

The full MOULI-STUDIO engine: documents, spreadsheets, charts, a small database, presentations,
PDF and image handling, a chat and an agent that act only through a gate. Local-first; every
change goes into a hash-chained audit log and risky operations need a second person's approval.

One `lib.rs` of inline modules (limits, error, crypto, audit, approval, workspace, word, render,
interchange, spreadsheet, charts, database, presentation, image, pdf, productivity, model_chat,
agent, studio_ui, cli, hardening) and a thin `mouli-studio` binary.

This is not the same crate as `mouli-studio`. `mouli-studio` is the small document model that the
Studio screen in `mouli-apps` and `mouli-device` uses (behind the `office` feature). Nothing in the
OS uses this suite yet.

## Command line

```sh
cargo run -p mouli-studio-suite -- help
cargo run -p mouli-studio-suite -- new notes.md "Quarterly plan"
cargo run -p mouli-studio-suite -- convert notes.md --to docx --out notes.docx
cargo run -p mouli-studio-suite -- export notes.md --to pdf --out notes.pdf
```

Checked: new, convert to DOCX, export to PDF, HTML and TXT. A PDF was read back with `pdftotext`
and a DOCX opens as a valid ZIP with Word XML. ODT export is refused.

## Not verified

- Opening the output in LibreOffice or Microsoft Office (`tools/interop.sh` skips without LibreOffice).
- Spreadsheet, chart, database, presentation, image and agent features beyond the unit tests.
- The chat path with a real model (the tests use a fake one).
- Miri, sanitizers, coverage-guided fuzzing and a dependency audit.
- Time-of-check/time-of-use races in the path checks (see `hardening::SECURITY_REVIEW`).

## Dependencies outside the workspace

`miniz_oxide`, `png`, `jpeg-decoder`, `jpeg-encoder` (all default features off). They are not
audited or fuzzed here. `missing_docs` is allowed in this crate because the source was imported in
one piece; field and variant docs are still to be written.

## Tests

```sh
cargo test -p mouli-studio-suite    # 175 tests
cargo clippy -p mouli-studio-suite --all-targets
```
