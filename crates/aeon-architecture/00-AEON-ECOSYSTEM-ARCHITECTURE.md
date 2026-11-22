# AEON Ecosystem — Master Architecture and Product Constitution

## 1. Vision

AEON is a unified AI-native operating ecosystem. Its products share common authorities and infrastructure while retaining clear domain ownership.

The architecture must avoid duplicate identity systems, competing databases, multiple agent runtimes and overlapping business authorities.

## 2. Canonical platform ownership

| Component | Sole responsibility |
|---|---|
| **AEON** | AI/model runtime, compute and hardware execution |
| **AEON-AUTH (AEON-AUTH)** | Enterprise identity, authentication, authorization, tenants, organizations, company/legal entity/plant hierarchy, policy, workflow, approvals and audit governance |
| **AEON-DATA** | Common persistence and data intelligence engine: relational, document, graph, vector, full-text, time-series, object, events and native AEON-RAG |
| **AEON-AGENTS** | Agent registry, factory, teams, skills, tools, workflow design, governance, evaluations, operations centre and marketplace |
| **AgentBox** | Actual agent execution, sessions, runtime lifecycle, orchestration and tool execution; integrated by AEON-AGENTS |
| **AEON applications** | Their own domain-specific business logic and user experiences |

## 3. AEON-RAG is part of AEON-DATA

AEON-RAG is not a separate top-level product. It is an internal AEON-DATA subsystem.

AEON-DATA includes:
- Relational and transactional storage.
- Document and key-value storage.
- Graph database.
- Vector database.
- Full-text and hybrid search.
- Time-series and event storage.
- Object/blob storage.
- Knowledge ingestion and parsing.
- Chunking and embedding pipelines.
- Semantic, vector and graph retrieval.
- GraphRAG and multi-hop retrieval.
- Reranking and context assembly.
- Evidence, citation and provenance.
- Semantic memory primitives.
- Retrieval evaluation and evolutionary optimization.

AEON-DATA is the common data substrate. AEON-AUTH governs who can access data. Applications own their domain records and business rules.

## 4. AgentBox relationship

AgentBox is the existing execution backbone. AEON-AGENTS must inspect and reuse `https://github.com/openwidgets/AgentBox`.

AEON-AGENTS owns agent definitions, configuration, registry, skills, tools, teams, permissions, evaluations, marketplace and management UX. AgentBox executes actual agent runs. Do not build a competing execution engine.

## 5. Product portfolio

### Foundation
- AEON
- AEON-AUTH
- AEON-DATA

### AI and development
- AEON-AGENTS (powered by AgentBox)
- AEON-STUDIO
- AEON-CODE
- AEON-CLOUD

### Productivity and communication
- AEON-WORK
- AEON-CONNECT
- AEON-MAIL
- AEON-SYSTEM

### Business and finance
- AEON-CRM
- AEON-ERP
- AEON-FINANCE
- AEON-COMMERCE

### Intelligence and security
- AEON-INTELLIGENCE
- AEON-SECURITY

### Industry applications
- AEON-EDU
- AEON-HEALTH
- AEON-LOGISTICS
- AEON-LEGAL
- AEON-INSURE

## 6. Domain boundaries

- AEON-CRM owns customer relationship processes; it does not own enterprise identity.
- AEON-ERP owns enterprise operations and corporate accounting.
- AEON-FINANCE owns financial institution, lending, banking, payments, markets, treasury, investment and asset-management domain processes.
- AEON-COMMERCE owns commerce storefront, marketplace and order experiences; it integrates with ERP for inventory/accounting and Finance for regulated payment/financial services.
- AEON-INTELLIGENCE owns analytics and decision-support experiences; AEON-DATA owns data storage and retrieval.
- AEON-SECURITY owns security product workflows; AEON-AUTH remains the identity and authorization authority.
- AEON-WORK owns productivity experiences; AEON-DATA owns persistence.
- Industry applications use shared authorities and must not recreate them.

## 7. Common engineering constitution

For each AEON product application:
- One Rust executable.
- Exactly one Rust source file: `src/main.rs`.
- Logical internal modules may be organized as Rust modules, types, traits, structs, enums and functions within that file.
- No separate Rust source files, microservices, sidecars, Electron or Flutter runtimes.
- External systems and existing platforms may remain separate and be integrated through documented adapters. AgentBox is the explicit example.
- Inspect existing code before modifying it.
- Reuse working implementations.
- No fake completion or unsupported production claims.
- Evidence-backed tests and readiness reporting after each iteration.

## 8. Shared platform contracts

Define and version:
- Identity and authorization contract.
- Tenant and organization context contract.
- Data access and schema contract.
- Audit event contract.
- Workflow and approval contract.
- Agent invocation and execution contract.
- Event and notification contract.
- File/document contract.
- Integration adapter contract.
- Observability and health contract.

All cross-product operations must carry tenant context, actor identity, authorization decisions, correlation IDs and audit metadata where applicable.

## 9. Data and transaction rules

- Use AEON-DATA as the common persistence engine.
- Use AEON-AUTH to govern authorization and enterprise data access.
- Use exact decimal or integer minor-unit representations for money; never binary floating point for authoritative money calculations.
- Use idempotency for retried state-changing operations.
- Preserve append-only financial/audit history where required.
- Use explicit state machines and validated transitions.
- Treat graph/vector/search indexes as derived or synchronized representations unless a domain explicitly defines otherwise.
- Define backup, recovery, retention, encryption and migration policies.

## 10. Security and safety

- Default deny; least privilege.
- Tenant isolation at every access boundary.
- Secrets must not be embedded in source code or prompts.
- High-impact actions require explicit authorization and approval.
- AI-generated suggestions must be distinguishable from verified facts and executed actions.
- External integrations must use official, authorized APIs.
- Log security-relevant actions without unnecessarily exposing sensitive data.

## 11. Delivery governance

Every repository implementation starts with:
1. Repository identity and branch verification.
2. Source and deployment audit.
3. Feature reuse/gap matrix.
4. Architecture boundary check.
5. Risk and migration plan.

Every iteration ends with:
- Overall readiness percentage and method.
- Subsystem-level status.
- Test evidence.
- Real versus mocked integration status.
- Security and data integrity findings.
- Blockers and next actions.

A prompt, folder, UI, scaffold or successful compile alone does not establish production readiness.
