# MOULI-STUDIO — Master Engineering Specification

The complete user-provided engineering brief is the source of truth for the product scope. It defines MOULI-STUDIO as one integrated, local-first Rust desktop monolith and executable, with exactly one Rust source file at src/main.rs.

## Non-negotiable architecture
- One Rust source file only: src/main.rs. No Rust modules, separate frontend/backend, microservices, sidecars, generated source, or external application logic.
- One primary executable: mouli-studio.
- Rust-native desktop UI; Tokio, egui/eframe, wgpu, Serde, Axum optional embedded API, FFmpeg integration, established format libraries, MOULI-DB preferred with SQLite fallback, and tracing.
- Local/offline editing must remain usable without cloud inference. Private content must not be sent to external AI providers without explicit authorization.

## Unified product scope
One coherent workspace with navigation, multi-document tabs, file manager, global search, assistant, properties/layers/timeline/history panels, command palette, drag/drop, themes and keyboard shortcuts.

Content engines required by the brief:
1. Word processor: rich text, page layout, styles, tables, citations, comments, tracked changes, DOCX/ODT/RTF/TXT/Markdown/HTML interoperability and AI writing.
2. Spreadsheet: workbook/sheets, deterministic formulas and recalculation, charts, pivots, financial analysis, XLSX/ODS/CSV/TSV and AI assistance.
3. Presentation: editable slides, masters, themes, media, notes, presenter mode, PPTX/ODP/PDF and AI generation/conversion.
4. PDF: viewing, annotations, page operations, OCR, forms, signatures, verified permanent redaction, import/export and document Q&A.
5. Photo/image: layers, masks, selections, brushes, retouching, color tools, compositing, RAW-capable decoding, reversible AI-assisted operations and common raster formats.
6. Vector/graphic design: paths, Bézier tools, shapes, typography, artboards, components, SVG/PDF and editable templates.
7. Video: multitrack non-linear timeline, clips, keyframes, transitions, codecs, FFmpeg, captions, scene detection and editable AI-assisted timelines.
8. Audio/podcast: multitrack waveform editing, recording, mixing, cleanup, transcription, chapters and common audio formats.
9. Animation/motion graphics: keyframes, paths, transforms, text/shape animation, compositing and video/GIF export.
10. Knowledge workspace: nested pages, databases, linked records, boards, calendars, kanban, backlinks, tags and semantic search.

## Shared platform capabilities
- Versioned .moulistudio project container, type-specific internal models, autosave, recovery, templates, metadata, comments, annotations, version history, sharing and export.
- Universal asset library with metadata, tags, versioning, deduplication, collections, previews and reusable references.
- Cross-format conversion with explicit disclosure of lossy transformations.
- Provider-independent AI layer supporting OpenAI-compatible, Anthropic-compatible, Ollama, local model servers, vision/image/speech and configurable video providers.
- Validated creative agents with scoped permissions, approvals, audit trail, undo, revocable credentials; never run arbitrary AI-generated shell commands or Rust.
- MOULI-DB preferred persistence, SQLite fallback, filesystem for large media, integrity checks, backup/restore and offline workflows.
- CPU fallback, optional GPU acceleration, bounded memory, background exports, observability, secure decoding, credential protection, access control and privacy controls.
- Collaboration readiness inside the monolith, local-first availability and structured observability without logging secrets or document contents.

## Delivery discipline
Implement incrementally, compile continuously, use mature libraries for complex formats/codecs, and test real interoperability. Do not claim format fidelity, AI integration, GPU acceleration, editing engines or artifact creation without evidence. Do not leave TODO, todo!() or unimplemented!() in required production paths. Features must be classified as implemented, tested, partial, not implemented or blocked.

## Required implementation sequence
Bootstrap; workspace/file manager; common document model; word processor; spreadsheet; presentation; PDF; image; vector; video; audio; animation; asset library; conversion; AI providers; creative assistant; agents; MOULI-DB; performance/GPU; security/privacy; tests; deployment.

## Production-readiness gate
Evidence required includes release build, functional navigation, DOCX/XLSX/PPTX tests, PDF editing/redaction verification, image layers/compositing, editable vector tests, video timeline/export, audio processing, animation rendering, conversion interoperability, actual AI provider integration, agent permission enforcement, asset persistence, measured GPU acceleration, recovery, security assessment and reproducible benchmarks. Readiness must be based on evidence, not screens or lines of code.

## Initial scope boundary
The first commit is a buildable foundation only. It must not be represented as a completed office/creative suite. Every engine beyond the document/knowledge text workspace remains pending until implemented and tested.
