# MOULI app layer: native apps on the AI-native OS

Status: plan only. Nothing here is implemented or verified.

## Focus

MOULI is a lean, AI-native operating system for **x86_64 laptops (CPU and GPU)**. On top of the
engine and model services, MOULI ships its own native apps, so that the whole experience (chat,
files, communication, browsing, agents, calls) runs on one local stack, offline-first, with the
assistant able to operate every app through the same governed interface.

## Design rules

1. **One action interface.** Every app exposes capabilities to the agent layer through TOPSTAR
   intent/action/result/evidence records. A user and an agent use the same gate. Nothing an agent does
   bypasses the permission prompt or the audit log.
2. **Local first.** Each app works with no network. Sync and remote features are additive.
3. **MOULI owns the shell, not every engine.** Where a mature open component exists (web engine, video
   codecs), MOULI embeds it and owns the surrounding code (tabs, permissions, storage, agent hooks).
4. **Identity and audit stay with MOULI-AUTH** online, and with a local single-user key and local
   append-only log offline (see the integration inventory).
5. **Lean.** Each app is a small service or process under moulid with a sandbox profile. No app
   runs unsandboxed.

## Apps and approach

| App | What it does | Approach | Depends on |
|---|---|---|---|
| Chat / assistant | Conversation, history, model picker with S and hardware fit | LibreOne UI over the local engine (PR 5) | engine, model services |
| Files | Browse, create and edit documents, spreadsheets and slides; undo snapshots | MOULI file service with content-addressed snapshots; document formats written by MOULI's own converters (docx/xlsx/pptx are open formats); agent can create files via actions | sandbox (PR 4) |
| File sharing | Send files and folders to another MOULI device or user | Own transfer protocol over TLS between devices (LAN discovery first, relay later); end-to-end keys from the local identity; resumable chunks with hashes | identity, network |
| Communication | Messages and group chat | Own E2EE message protocol on the same transport as sharing; store-and-forward when offline | identity, network |
| Browser | Web browsing | **Embed an existing web engine** inside an MOULI shell: MOULI-owned tabs, per-site permissions, download and file hooks, agent actions (read page, fill form with approval). Engine choice (Chromium/CEF, Servo or WebKit) decided by a measured spike on size, GPU use and sandboxing | GPU, sandbox, network |
| Agents and scheduled tasks | Run agents now or on a schedule (daily, on event); results and approvals in chat | Scheduler inside moulid (cron-like, persisted); AgentBox manifests via TOPSTAR (PR 4); every run produces an evidence record; tasks run offline with local models | PR 4 |
| Video and voice calls | 1:1 and small group calls | Embed a WebRTC stack with hardware video codecs where drivers allow; MOULI owns signalling over its own transport; camera/mic only through permission prompts | GPU, network, identity |

## Order (after the engine track and PR 3)

1. PR 4 agents and sandbox, including the scheduler and local identity/audit.
2. PR 5 chat UI.
3. Files app and file creation.
4. Transfer protocol, then messaging on top of it.
5. Browser spike, then integration.
6. Calls.

Each step is its own PR with tests and a stated "done when".

## Feasibility and risks (stated up front)

- A browser engine and a video stack are the largest items. Embedding keeps them to months, not years,
  but adds third-party code and a sandboxing and update burden.
- GPU video encode and decode on arbitrary laptops depends on drivers. This is a PR 6 risk and cannot
  be verified in CI.
- E2EE protocol design needs an external security review before it is trusted. Until then it is
  marked unaudited.
- Laptop battery, thermal and suspend behaviour are unverified until tested on real hardware.
