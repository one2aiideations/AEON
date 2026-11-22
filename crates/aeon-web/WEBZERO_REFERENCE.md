# WebZero v4.0.24 — supplied runtime reference

## What the uploaded notebook actually establishes

The supplied notebook is titled **WebZero v4.0.24 — Actual Runtime Colab** and names `webzero-v4.0.24.js` as the canonical runtime. Its sequence is:

1. Install Node.js, npm and curl in Colab.
2. Mount Google Drive and recursively locate the exact `webzero-v4.0.24.js` file.
3. Copy that file to `/content/webzero-v4.0.24.js`.
4. Require the real file and inspect the types of `WebZero` and `startWebZeroHttpServer`, plus the exported symbol names.
5. Stop previous launcher, Cloudflare and port-8080 processes.
6. Instantiate `new WebZero({ isolated: true, nodeId: 'colab_webzero' })`.
7. Call `runtime.start()` when exported.
8. Start the exported HTTP server with the documented notebook call shape `startWebZeroHttpServer(runtime, 8080)`.
9. Probe the local server at `/`, `/health`, `/healthz`, `/ready`, `/readyz` and `/metrics`.
10. Only after local checks, start a temporary Cloudflare Quick Tunnel and probe the same public paths.

The notebook explicitly says that if local checks return 404, fix the local WebZero server before troubleshooting Cloudflare.

## Migration facts and limits

- The notebook is a runtime launcher and smoke-test procedure; it is not the WebZero implementation source.
- The actual `webzero-v4.0.24.js` file is not among the uploaded files.
- The notebook's cells do not enumerate the complete exported API, route handlers, request/response schemas, persistence behavior or compiler semantics.
- The JavaScript runtime depends on Node.js in the supplied Colab procedure. AEON-WEB's target remains a native Rust monolith with no Node.js runtime dependency.
- Cloudflare Quick Tunnel is a temporary exposure/test mechanism in the notebook, not a required AEON-WEB runtime dependency.

## Required next migration gate

Obtain the exact canonical `webzero-v4.0.24.js` artifact and its relevant fixtures/tests. Then inventory exports, classes, functions, routes, methods, schemas, error semantics, lifecycle and persistence. Build a traceability matrix and behavioral regression fixtures before claiming compatibility or completing the compiler port.
