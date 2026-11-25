# MOULI Ecosystem — Master Architecture and Product Constitution

## 1. Vision

MOULI is a unified AI-native operating ecosystem. Its products share common authorities and infrastructure while retaining clear domain ownership.

The architecture must avoid duplicate identity systems, competing databases, multiple agent runtimes and overlapping business authorities.

## 2. Canonical platform ownership

| Component | Sole responsibility |
|---|---|
| **MOULI** | AI/model runtime, compute and hardware execution |
| **MOULI-AUTH** | Enterprise identity, authentication, authorization, tenants, organizations, company/legal entity/plant hierarchy, policy, workflow, approvals and audit governance |
| **MOULI-DATA** | Common persistence and data intelligence engine: relational, document, graph, vector, full-text, time-series, object, events and native MOULI-RAG |
| **MOULI-AGENTS** | Agent registry, factory, teams, skills, tools, workflow design, governance, evaluations, operations centre and marketplace |
| **AgentBox** | Actual agent execution, sessions, runtime lifecycle, orchestration and tool execution; integrated by MOULI-AGENTS |
| **MOULI applications** | Their own domain-specific business logic and user experiences |

## 3. MOULI-RAG is part of MOULI-DATA

MOULI-RAG is not a separate top-level product. It is an internal MOULI-DATA subsystem.

MOULI-DATA includes:
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

MOULI-DATA is the common data substrate. MOULI-AUTH governs who can access data. Applications own their domain records and business rules.

## 4. AgentBox relationship

AgentBox is the existing execution backbone. MOULI-AGENTS must inspect and reuse `https://github.com/openwidgets/AgentBox`.

MOULI-AGENTS owns agent definitions, configuration, registry, skills, tools, teams, permissions, evaluations, marketplace and management UX. AgentBox executes actual agent runs. Do not build a competing execution engine.

## 5. Product portfolio

### Foundation
- MOULI
- MOULI-AUTH
- MOULI-DATA

### AI and development
- MOULI-AGENTS (powered by AgentBox)
- MOULI-STUDIO
- MOULI-CODE
- MOULI-CLOUD

### Productivity and communication
- MOULI-WORK
- MOULI-CONNECT
- MOULI-MAIL
- MOULI-SYSTEM

### Business and finance
- MOULI-CRM
- MOULI-ERP
- MOULI-FINANCE
- MOULI-COMMERCE

### Intelligence, security and compliance
- MOULI-INTELLIGENCE
- MOULI-SECURITY
- MOULI-COMPLIANCE

### Scientific discovery and engineering
- MOULI-MATERIALS
- MOULI-CHEMISTRY
- MOULI-DRUG
- MOULI-SOLVER
- MOULI-QUANTUM

### Industry applications
- MOULI-EDU
- MOULI-HEALTH
- MOULI-LOGISTICS
- MOULI-LEGAL
- MOULI-INSURE

## 6. Domain boundaries

- MOULI-CRM owns customer relationship processes; it does not own enterprise identity.
- MOULI-ERP owns enterprise operations and corporate accounting.
- MOULI-FINANCE owns financial institution, lending, banking, payments, markets, treasury, investment and asset-management domain processes.
- MOULI-COMMERCE owns commerce storefront, marketplace and order experiences; it integrates with ERP for inventory/accounting and Finance for regulated payment/financial services.
- MOULI-INTELLIGENCE owns analytics and decision-support experiences; MOULI-DATA owns data storage and retrieval.
- MOULI-SECURITY owns security product workflows; MOULI-AUTH remains the identity and authorization authority.
- MOULI-MATERIALS, MOULI-CHEMISTRY and MOULI-DRUG own autonomous discovery campaigns, candidates, evidence and reports in their domains. They share one rule set: every candidate is screened for hazard, toxicity and controlled substances before it is shown or ranked, predictions are labelled as predictions, and any synthesis, purchase or laboratory request needs a recorded human approval. They use MOULI-AGENTS for agents, MOULI-AUTH for approvals and audit, MOULI-DATA for storage, and external scientific engines only through adapters.
- MOULI-SOLVER owns hard-problem workbenches: formal statements, experiments and an evidence ledger that never mistakes computed evidence for a machine-checked proof.
- MOULI-QUANTUM owns quantum programs, simulations and hardware job records; hardware runs need a budget and an approval.
- MOULI-COMPLIANCE owns regulatory frameworks, obligations, controls, evidence, assessments, privacy registers and compliance reporting; it uses MOULI-AUTH for workflow, approvals and audit, MOULI-DATA for persistence and MOULI-AGENTS for workflow design and agents, and does not certify compliance or give legal advice.
- MOULI-WORK owns productivity experiences; MOULI-DATA owns persistence.
- Industry applications use shared authorities and must not recreate them.

## 7. Common engineering constitution

For each MOULI product application:
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

- Use MOULI-DATA as the common persistence engine.
- Use MOULI-AUTH to govern authorization and enterprise data access.
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
