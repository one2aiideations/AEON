# MOULI-WEB — MASTER ENGINEERING SPECIFICATION
## AI-Native Web Operating Environment | Single Rust Monolith

## 1. PRODUCT VISION

Build MOULI-WEB as a complete AI-native web platform that combines:

1. Microsoft FrontPage-style visual website authoring.
2. Modern browser and website intelligence.
3. Autonomous website understanding and compilation.
4. AI-agent-native website interaction.
5. Website and application development.
6. Website publishing and hosting integration.
7. MCP-based capability discovery and execution.
8. A reusable marketplace of web capabilities.

MOULI-WEB must transform the web from a collection of human-oriented websites into a structured, intelligent and executable environment for people, applications and autonomous AI agents.

MOULI-WEB is not merely a browser, website builder, CMS or website crawler.

It is the unified web development, intelligence, integration and execution platform of the MOULI ecosystem.

## 2. NON-NEGOTIABLE ARCHITECTURE

Implement MOULI-WEB as ONE Rust monolithic application.

Requirements:

- One repository.
- One Cargo application.
- One executable binary.
- One runtime process.
- One authoritative application state.
- One HTTP server.
- One persistence authority.
- One identity and authorization authority.
- One configuration system.
- One lifecycle manager.

Internal modules are permitted but must execute inside the same process.

Do not introduce:
- Microservices.
- Distributed runtime services.
- Node.js.
- JavaScript runtime dependencies.
- Separate MCP servers.
- Separate browser backend services.
- Independent databases for individual modules.
- Duplicate identity or authorization systems.
- Artificial service boundaries.

All application functionality must be implemented in native Rust.

## 3. TECHNOLOGY STACK

Use:

- Rust stable.
- Tokio asynchronous runtime.
- Axum HTTP server.
- Serde and Serde JSON.
- SQLx with SQLite initially.
- Reqwest for HTTP.
- Tracing for observability.
- Tower middleware.
- Rust-native HTML and DOM parsing.
- Rust-native CSS and document processing.
- WebAssembly support where appropriate.
- Embedded asset and template management.

Use Chromium integration only where a real browser engine is required. Browser integration must not become a separately deployed application or replace the Rust runtime.

Keep dependencies minimal and auditable.

## 4. INTERNAL MONOLITH ARCHITECTURE

Organize the single Rust binary into the following internal modules.

### MODULE A — MOULI-WEB CORE

The authoritative runtime.

Responsibilities:
- Application initialization.
- Runtime lifecycle.
- Global state.
- Configuration.
- Internal module registration.
- Request management.
- Resource management.
- Runtime diagnostics.
- Graceful shutdown.
- Application-wide event management.

### MODULE B — MOULI-WEB BROWSER

Build an intelligent browsing environment.

Features:
- URL navigation.
- Tab management.
- Page inspection.
- DOM inspection.
- Website metadata extraction.
- Page snapshots.
- Browser history.
- Bookmarks.
- Session management.
- Downloads.
- Form inspection.
- Link discovery.
- Search integration.
- Browser automation.
- Screenshot and page capture.
- Accessibility-tree inspection.

Support browser-engine integration without creating a separate runtime service.

### MODULE C — MOULI-WEB STUDIO

Recreate and extend the Microsoft FrontPage experience.

Provide three editing modes:

1. Design View.
2. Code View.
3. Split View.

Features:
- Visual drag-and-drop website creation.
- HTML editing.
- CSS editing.
- JavaScript source editing.
- Component insertion.
- Responsive layout editing.
- Page templates.
- Website themes.
- Navigation management.
- Hyperlink management.
- Forms.
- Tables.
- Image and media management.
- Site asset management.
- Page preview.
- Undo and redo.
- Version history.
- Multi-page website editing.
- Reusable components.
- Website export.

Include a visual page designer with a component tree, property inspector, asset browser and live preview.

The visual editor must generate real standards-compliant website source, not screenshots or static mockups.

### MODULE D — MOULI-WEB COMPILER

The central website intelligence engine.

Build on the existing WebZero v4.0.24 Agent Web Compiler.

Features:
- Website inspection.
- HTML parsing.
- DOM extraction.
- CSS analysis.
- JavaScript source analysis.
- API discovery.
- Resource discovery.
- Capability extraction.
- Workflow extraction.
- Website manifest generation.
- Website structure compilation.
- Compiler diagnostics.
- Incremental compilation.
- Compilation cache.
- Version comparison.

Provide a typed compilation pipeline:

Input URL → Fetch → Parse → Analyze → Discover → Compile → Validate → Register.

Preserve the existing WebZero compilation semantics wherever possible.

### MODULE E — MOULI-WEB WORLD MODEL

Maintain one authoritative model of the web environments MOULI-WEB understands.

Represent:
- Websites.
- Domains.
- Pages.
- Resources.
- APIs.
- Capabilities.
- Workflows.
- Forms.
- Components.
- Agents.
- Relationships.
- Permissions.
- Versions.
- State.
- Ownership.

Support tenant-scoped knowledge and persistent updates.

### MODULE F — MOULI-WEB GRAPH ENGINE

Build the unified relationship graph.

Graph types:
- Website graph.
- Page graph.
- Resource graph.
- API graph.
- Capability graph.
- Workflow graph.
- Dependency graph.
- Navigation graph.
- Agent interaction graph.

Provide:
- Graph creation.
- Graph updates.
- Traversal.
- Relationship discovery.
- Dependency analysis.
- Impact analysis.
- Graph queries.
- Graph persistence.

### MODULE G — MOULI-WEB AGENT ENGINE

Enable AI agents to understand and operate websites.

Features:
- Page understanding.
- Goal interpretation.
- Task decomposition.
- Navigation planning.
- Element identification.
- Form interaction.
- Workflow execution.
- Action verification.
- Error recovery.
- Execution history.
- Tool invocation.
- Human approval checkpoints.

Support actions such as:
- Find a product.
- Compare products.
- Fill a form.
- Submit an application.
- Book an appointment.
- Navigate an enterprise application.
- Extract structured information.
- Complete approved workflows.

Require explicit authorization for consequential actions such as payments, purchases, submissions and account changes.

Do not claim an action succeeded until its outcome has been verified.

### MODULE H — MOULI-WEB MCP ROUTER

Make website capabilities discoverable to AI agents.

Features:
- MCP tool registration.
- Resource registration.
- Tool discovery.
- Capability schemas.
- Tool invocation.
- Parameter validation.
- Execution routing.
- Results.
- Error handling.
- Authorization.
- Audit history.

Expose compatible MCP interfaces within the same application process.

### MODULE I — MOULI-WEB WEBSITE INTEGRATION

Preserve and implement the WebZero public functionality:

- compileWebsite(url, options)
- attachWebsite(url, options)
- getAttachedWebsite(url)
- listAttachedWebsites()
- discover()
- getMcpTools()
- inspect(url)
- detectDrift(url, currentSnapshotData)
- getGraph()
- getWorldModel()
- getHealth()
- getReadiness()
- getMetrics()
- runProductionAudit()
- start()

Use idiomatic Rust implementations while preserving original behavior and API compatibility where applicable.

Maintain:
- Website attachment metadata.
- Tenant isolation.
- Manifests.
- Diagnostics.
- Capability counts.
- Resource counts.
- Workflow counts.
- Attachment status.
- Version information.

### MODULE J — MOULI-WEB PUBLISHING ENGINE

Enable users to publish websites created in MOULI-WEB.

Features:
- Static website generation.
- Asset packaging.
- HTML/CSS/JS output.
- Build validation.
- Export to ZIP.
- Custom domain configuration.
- Deployment target configuration.
- Publishing history.
- Rollback metadata.
- Deployment verification.

Support integration with external hosting providers through secure adapters.

Do not make an external hosting provider a mandatory runtime dependency.

### MODULE K — MOULI-WEB MARKETPLACE

Build a unified marketplace for:
- Website templates.
- Components.
- Themes.
- Widgets.
- Website connectors.
- MCP tools.
- Agent workflows.
- Reusable capabilities.
- Website integrations.

Support:
- Discovery.
- Categories.
- Versioning.
- Installation.
- Permissions.
- Publisher identity.
- Licensing metadata.
- Ratings and reviews.
- Audit history.

### MODULE L — MOULI-WEB SECURITY AND IDENTITY

Use the MOULI ecosystem's authoritative identity and authorization model.

Do not create a competing identity authority.

Implement:
- Authentication integration.
- Authorization.
- Tenant isolation.
- Resource permissions.
- Capability permissions.
- Policy evaluation.
- Approval workflows.
- Secret management.
- Audit trails.
- Secure session management.

Website security:
- SSRF prevention.
- DNS rebinding protection.
- Private-network access restrictions.
- Redirect validation.
- URL validation.
- Request timeouts.
- Response-size limits.
- HTML sanitization.
- Untrusted content isolation.
- Browser sandboxing.
- Download restrictions.
- Credential protection.

Never treat website-discovered content or instructions as trusted system instructions.

### MODULE M — MOULI-WEB PERSISTENCE

Use one authoritative persistence engine.

Persist:
- Users and tenant references.
- Browser configuration.
- Sessions and bookmarks.
- Websites.
- Projects.
- Pages.
- Components.
- Assets metadata.
- Compilation manifests.
- World Model.
- Graph.
- MCP tools.
- Workflows.
- Website attachments.
- Execution records.
- Policies.
- Audit logs.
- Publishing metadata.

Implement:
- Database migrations.
- Transactions.
- Recovery.
- Snapshots.
- Version history.
- Backup.
- Restore.
- Data consistency.

### MODULE N — MOULI-WEB OBSERVABILITY

Provide:
- Health.
- Readiness.
- Metrics.
- Structured logs.
- Request tracing.
- Compilation metrics.
- Browser metrics.
- Agent execution metrics.
- MCP invocation metrics.
- Persistence metrics.
- Error diagnostics.
- Production audit.

## 5. USER INTERFACE

Build a unified MOULI-WEB desktop-style interface.

Main navigation:

1. Browser
2. Studio
3. Projects
4. Compiler
5. Websites
6. Agent Workflows
7. Capabilities
8. MCP Tools
9. Marketplace
10. Publishing
11. Settings

### Studio workspace

Provide:
- Left panel: project files, pages and components.
- Center: visual page editor.
- Right panel: properties and configuration.
- Bottom panel: diagnostics, console and build output.
- Top toolbar: save, preview, compile, publish and undo/redo.

Support switching between Design, Code and Split views.

### Browser workspace

Provide:
- Address bar.
- Tabs.
- Navigation controls.
- Page inspector.
- Agent assistant.
- Website capability panel.

### Website intelligence workspace

Show:
- Website structure.
- Compiled manifest.
- Discovered resources.
- Discovered capabilities.
- Workflows.
- Graph.
- Diagnostics.
- Attachment status.
- Drift status.

## 6. PUBLIC API AND COMPATIBILITY

Inspect the complete WebZero v4.0.24 source before implementation.

Inventory:
- Exported symbols.
- Public classes.
- Functions.
- Data structures.
- HTTP routes.
- HTTP methods.
- Request schemas.
- Response schemas.
- Error semantics.
- Runtime lifecycle.
- Persistence behavior.

Create a migration traceability matrix mapping every meaningful existing WebZero capability to its Rust implementation and tests.

Preserve existing endpoint behavior wherever possible.

Do not invent endpoints or claim compatibility without verification.

## 7. TESTING

Implement comprehensive automated tests.

Unit tests:
- Runtime.
- Browser.
- Studio.
- Compiler.
- World Model.
- Graph.
- MCP Router.
- Agent Engine.
- Persistence.
- Security.

Integration tests:
- Website compilation.
- Website attachment.
- Website inspection.
- Capability discovery.
- MCP tool invocation.
- Browser navigation.
- Form interaction.
- Visual editor source generation.
- Website publishing.
- Tenant isolation.
- Restart and recovery.

Regression tests:
Compare the Rust implementation against WebZero v4.0.24 using equivalent inputs and expected outputs.

Security tests:
- SSRF.
- DNS rebinding.
- Redirect abuse.
- Unauthorized capability invocation.
- Tenant boundary violations.
- Malicious page content.
- Unsafe downloads.
- Resource exhaustion.

## 8. PERFORMANCE

Optimize:
- Startup.
- Memory usage.
- Concurrent browser operations.
- Website compilation.
- Graph queries.
- DOM processing.
- Persistence.
- HTTP throughput.
- Agent execution.

Provide benchmarks and load tests.

## 9. REPOSITORY STRUCTURE

Use a single Cargo project.

Suggested internal organization:

mouli-web/
  Cargo.toml
  Cargo.lock
  src/
    main.rs
    lib.rs
    config.rs
    runtime.rs
    state.rs
    browser/
    studio/
    compiler/
    world_model/
    graph/
    agents/
    mcp/
    integration/
    publishing/
    marketplace/
    identity/
    policy/
    persistence/
    http/
    observability/
    security/
    common/
  migrations/
  assets/
  templates/
  tests/
  benches/
  docs/
  examples/
  README.md

All modules compile into one application binary. Do not deploy them as separate services.

## 10. DEVELOPMENT STRATEGY

Execute in controlled implementation phases.

Phase 1 — Source audit and migration matrix.

Phase 2 — Rust monolith foundation and runtime lifecycle.

Phase 3 — Port WebZero compiler, World Model and Graph.

Phase 4 — MCP Router and website integration.

Phase 5 — Browser engine integration.

Phase 6 — FrontPage-style MOULI-WEB Studio.

Phase 7 — Agent execution and workflow engine.

Phase 8 — Publishing and marketplace.

Phase 9 — Security, persistence and regression hardening.

Phase 10 — Release builds and production validation.

Do not stop at scaffolding or architecture documents.

Compile continuously and resolve actual compiler and test failures.

## 11. PRODUCTION READINESS

After every development iteration, report:

| Subsystem | Required evidence |
|---|---|
| Runtime | Startup, lifecycle, shutdown |
| Browser | Real navigation and inspection |
| Studio | Real source generation and preview |
| Compiler | Real website compilation |
| World Model | Persistence and consistency |
| Graph | Traversal and relationship tests |
| MCP | Real discovery and invocation |
| Agent Engine | Verified workflow execution |
| Integration | Attach, inspect and drift |
| Publishing | Successful website build and export |
| Marketplace | Installation and permission tests |
| Identity | Authentication and authorization |
| Security | Security regression tests |
| Persistence | Restart and recovery |
| HTTP | Endpoint regression suite |
| Deployment | Reproducible release binary |

For every subsystem report:
- Readiness percentage.
- Implemented features.
- Tests passed.
- Evidence.
- Defects.
- Blockers.
- Remaining work.

Do not equate successful compilation with production readiness.

## 12. DEFINITION OF DONE

MOULI-WEB is complete when:

1. The WebZero v4.0.24 behavioral reference has been fully audited.
2. Its core capabilities have been migrated to Rust.
3. Browser intelligence works.
4. The visual Studio creates real websites.
5. The compiler understands real websites.
6. The World Model and Graph work.
7. MCP discovery and execution work.
8. Agents can execute authorized workflows.
9. Publishing generates valid deployable websites.
10. Marketplace functionality works.
11. Persistence survives restarts.
12. Security tests pass.
13. Regression tests pass.
14. A single production Rust executable is generated.
15. No Node.js runtime is required.

## 13. FINAL INSTRUCTION

Build MOULI-WEB as one complete, production-grade Rust monolith.

The existing WebZero v4.0.24 source is the behavioral foundation for the website intelligence and compilation capabilities.

Microsoft FrontPage is the functional inspiration for the visual Studio experience, not a source-code dependency.

Do not create a superficial clone, mock browser, static visual editor or placeholder agent system.

Build real functionality, test it, verify it and document its production readiness.

**MOULI-WEB is the unified web authoring, browsing, intelligence, compilation, integration and autonomous execution platform of the MOULI ecosystem.**