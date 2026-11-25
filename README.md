# MOULI

MOULI is a lean, AI-native operating system for x86_64 laptops (CPU and GPU), written in Rust. It boots
from a Buildroot image in which `moulid` is PID 1 and llama.cpp is the local inference engine
(`os/README.md`). Everything runs through one governed interface: policy, approvals by a second
person for consequential actions, and a hash-chained audit log.

Status: early development. Nothing here is production-ready. The desktop, the applications and the
screens below are tested in software only; they are not yet part of the boot image, which still
runs `moulid`'s own console.

## Operating system and hardware

| Crate | Purpose |
|---|---|
| `moulid` | Init (PID 1), console, networking, Wi-Fi, persistence, inference-server supervisor |
| `mouli-hw` | Hardware probe and run plan |
| `mouli-update` | Signed OS updates, A/B slots, boot counting and rollback |
| `mouli-sdv` | Experimental software-defined-vehicle development environment (not safety-certified) |

## Models and inference

| Crate | Purpose |
|---|---|
| `mouli-llm` | The model engine in one crate: GGUF reader and writer (`gguf`), dequantisation (`quant`), tokenizer, tensors, weight storage with cache, expert store and block sources (`weights`), CPU kernels and the forward pass, the theory layer (`theory`: robust score, gap law, calibration, cascade, TIES merge), weight analysis (`compress`) and selective-loading plans (`sparse`: hot expert sets, neuron slicing, memory tiers) |
| `mouli-hosted` | Hosted-model chat client (streaming) over MOULI's own HTTPS |
| `mouli-http`, `mouli-tls`, `mouli-pki`, `mouli-crypto` | Own HTTP/1.1 client, TLS 1.3 client, X.509 verification, crypto primitives |

## Governed interface and applications

| Crate | Purpose |
|---|---|
| `mouli-interface` | The governed workspace: sessions, intents, policy, approvals, data washing, routing, audit |
| `mouli-apps` | Sheet, Studio, CRM and Code as governed capabilities |
| `mouli-sheet`, `mouli-studio` | Spreadsheet core; documents (the other engines are fail-closed stubs) |
| `mouli-crm` | CRM domain: leads, pipeline with approvals, cases, renewals, consent |
| `mouli-code` | Unified-diff engine, patch policy, atomic apply and exact rollback |
| `mouli-project` | Project management: tasks, dependencies, critical path, kanban |
| `mouli-mail`, `mouli-web` | Mail server, browser agent layer |
| `mouli-biz`, `mouli-business`, `mouli-ops`, `mouli-data`, `mouli-system` | Business, operations, data and control-centre applications |

## Screens and desktop

| Crate | Purpose |
|---|---|
| `mouli-screen` | Screens as dependency-free state machines: chat, models, approvals, audit, security, review, sheet, studio, code editor |
| `mouli-desk` | Classic desktop (windows, menu bar, icons, pointer) drawn in software |
| `mouli-device` | Controller joining the screens to the governed workspace |
| `mouli-shell` | Runs the screens on a Linux framebuffer and keyboard |
| `mouli-player` | Video and YouTube player: safe address parsing, queue, hands-free Shorts autoplay, player state machine, mpv control, local bridge to YouTube's own player; has a screen (F10) but no decoder, browser engine or sound output in the image yet |

## Agents, memory and research

| Crate | Purpose |
|---|---|
| `mouli-agentic` | Everything about agents in one crate: object model and event bus (`kernel`), local retrieval, knowledge graph and ontology (`memory`), agent-memory primitives (`agentmem`), cognitive primitives (`cognition`), multi-agent control plane (`orchestra`), scheduled jobs (`jobs`), the agent loop (`runtime`) sibling-system adapters and the AI data graph (`graph`: conversations, messages, models, files, tool calls and approvals as typed, timed nodes and edges, saved as an append-only log) |
| `mouli-systems` | The platform services in one crate: model gateway (`gateway`), identity (`identity`, `auth`), security (`security`, `gate`), scheduler (`sched`), system state (`store`), integrations (`topstar`, `comms`, `mesh`), monitoring (`monitor`), billing (`billing`), bring-your-own-key vault (`keys`), data washing with GDPR and DPDP profiles (`wash`) and privacy duties (`privacy`) |
| `mouli-forge`, `mouli-neuro` | Versioned agent manifests, spiking runtime |
| `mouli-lab` | Aetheron research harness with gradient checks (no real-data results yet) |

## Product specifications (no code yet)

`mouli-architecture`, `mouli-chemistry`, `mouli-cloud`, `mouli-commerce`, `mouli-compliance`, `mouli-connect`,
`mouli-drug`, `mouli-edu`, `mouli-erp`, `mouli-finance`, `mouli-health`, `mouli-insure`, `mouli-intelligence`,
`mouli-legal`, `mouli-logistics`, `mouli-master-prompts`, `mouli-materials`, `mouli-quantum`, `mouli-solver` and
`mouli-work` are specification folders only.

## Build and test

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
os/build.sh        # the boot image (needs Buildroot and the musl target; see os/README.md)
```
