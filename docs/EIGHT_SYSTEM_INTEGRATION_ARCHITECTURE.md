# Eight-System Integration Architecture

Status: integration contract proposal with Phase 1 TOPSTAR source pin committed; source repositories remain independently versioned.

## Scope (fixed)

This integration includes exactly these eight CMKIXVI repositories:

| System | Repository | Authoritative responsibility |
|---|---|---|
| MOULI | `CMKIXVI/MOULI` | Operating-system, hardware, runtime and workload execution authority |
| MOULIONE | `CMKIXVI/moulione` | Enterprise identity, authorization, tenant/company/plant, persistence, workflow, approval, audit and business-domain authority |
| LibreOne | `CMKIXVI/LibreOne` | Conversational workspace, multimodal interaction and user-facing artifacts |
| AgentBox | `CMKIXVI/AgentBox` | Agent lifecycle, execution coordination, capability contracts and evaluation |
| YNDRA | `CMKIXVI/yndra` | Intelligence, cognition and language abstractions; historical recovery remains provenance-first |
| TOMATOAI | `CMKIXVI/TOMATOAI` | Agricultural intelligence and supervised robotics vertical |
| TOPSTAR | `CMKIXVI/topstar` | Cross-system capability, responsibility, intent/action/result/evidence protocol |
| AetherLang | `CMKIXVI/AetherLang` | Heterogeneous-compute language, typed IR, compiler, lowering and backend/device orchestration |

No other repository is in this consolidation scope.

## Non-negotiable authority boundaries

1. **MOULI executes.** It owns OS/runtime, hardware access, resource scheduling and workload execution. It does not become a second enterprise identity or business-data authority.
2. **MOULIONE governs enterprise access and records.** It is the sole authority for enterprise identity, authorization, tenants, company/plant scope, persistence, business workflows, approvals, audit and business permissions.
3. **LibreOne presents.** It is a workspace and interaction surface. It must not mint enterprise identity, own parallel tenant/business persistence, or independently become the model-execution authority.
4. **AgentBox manages agents.** Agent lifecycle, execution/evaluation and agent capability coordination belong here. Enterprise actions require MOULIONE authorization; physical/device execution additionally requires MOULI/TOPSTAR policy gates.
5. **YNDRA defines cognition and language abstractions.** Compiler/IR responsibilities are coordinated with AetherLang. The historical `recovered/` corpus is immutable, provenance-first input—not production code to bulk-import.
6. **TOPSTAR defines shared protocol and responsibility contracts.** It does not create a competing OS, general runtime, identity provider or enterprise database. Its contracts carry actor, intent, context, capability, responsibility, policy, limits, evidence, verification, revocation and audit references.
7. **AetherLang compiles heterogeneous workloads.** It owns language, typed IR, target knowledge, lowering, backend selection and device orchestration contracts. MOULI remains the execution authority.
8. **TOMATOAI is a vertical application.** It consumes shared capabilities. Its safety supervisor remains independent and authoritative for physical-action constraints; model/VLA output is a proposal, never authorization.

## Integration sequence

### Phase 0 — establish the contract baseline
- Record each upstream repository, default branch, source commit and license evidence before importing or pinning it.
- Define versioned cross-system API/schema contracts and compatibility policy.
- Add provenance records for every imported artifact.
- Do not flatten repositories or rewrite their histories.

### Phase 1 — MOULI + TOPSTAR
- [x] Pin TOPSTAR at an immutable upstream commit and record Apache-2.0 license evidence.
- [x] Add a source-level contract/security mapping; preserve MOULI as the execution authority.
- [x] Add an MOULI-side bounded C adapter DTO and structural validator with host-injected authentication verification.
- [ ] Define canonical wire serialization and cryptographic signing/verification adapter.
- [ ] Map TOPSTAR capability, responsibility, policy, action/result, evidence and verification contracts onto MOULI's runtime boundaries.
- Reuse protocol definitions only after overlap review.
- Keep MOULI as the sole runtime/execution authority.
- Add conformance fixtures for authorization denial, capability expiry/revocation, resource limits, cancellation, evidence completeness and verification failure.

### Phase 2 — YNDRA + AetherLang
- [x] Audit current YNDRA semantic IR/architecture-lowering modules and AetherLang architecture/source-recovery documentation.
- [x] Record the proposed semantic-IR → hardware-workload-IR ownership split and overlap risks.
- [ ] Resolve explicit licenses for both repositories before source pinning/copying; YNDRA has no root license evidence and AetherLang states no license has been selected.
- [ ] Recover AetherLang's historical source export with hashes/provenance before implementation claims.
- Define ownership of language syntax, semantic model, typed IR, optimization passes, target profiles and backend lowering.
- YNDRA supplies cognition/language abstractions; AetherLang owns heterogeneous compilation and target lowering.
- MOULI consumes execution-ready workload artifacts and receipts.
- Preserve YNDRA recovery data without normalizing or silently promoting it.

### Phase 3 — MOULIONE + AgentBox
- [x] Audit current MOULIONE/AgentBox authority boundaries and AgentBox execution/sandbox limitations.
- [x] Define the proposed audience-bound, tenant/company/plant-scoped authorization handoff and business-action return path.
- [ ] Resolve root license evidence/policy for both repositories before source pins or copying.
- Define a short-lived, tenant-scoped authorization handoff from MOULIONE to AgentBox.
- Bind every agent run, tool call and result to actor, tenant/company/plant, capability, policy decision and audit correlation.
- No AgentBox bypass of MOULIONE business authorization; no credentials persisted in workspace or agent manifests.
- Mutating business operations require explicit policy/approval gates and idempotency.

### Phase 4 — LibreOne
- [x] Pin LibreOne at the exact reviewed integration PR #1 head; verify Apache-2.0 license text.
- [x] Document its nested OOPSOne dependency as excluded and require non-recursive submodule checkout.
- Connect workspace sessions to MOULIONE's trusted session and chat APIs.
- Pass host-issued credentials per request; do not create a second identity or enterprise conversation store.
- Keep provider/model execution behind the governed service boundary.
- Validate real identity-provider and live service integration separately from mocked client tests.

### Phase 5 — TOMATOAI
- [x] Audit the VLA proposal interface and current pure scalar safety helpers.
- [x] Document the independent physical-safety and MOULI execution boundary.
- [ ] Resolve root license evidence before source pinning/copying.
- Integrate perception, plant registry/state, analytics, typed task proposals and robotics adapters through versioned contracts.
- Enforce safety supervisor checks for schema, freshness, authorization, reachability, workspace, speed/force, collision clearance, timeout and emergency stop.
- Verify actions from observed outcomes; never treat model output as permission to actuate.

## Cross-cutting contract requirements

Every cross-system request must carry:
- contract name and semantic version;
- correlation and idempotency identifiers where applicable;
- authenticated actor/service identity and authorization decision reference;
- tenant/company/plant scope for enterprise data;
- declared capability and responsibility;
- resource, time, network, financial and physical limits relevant to the action;
- provenance for source/model/compiler/backend inputs;
- structured result, failure and verification evidence.

Secrets are resolved at runtime from an approved secret manager. No secret values, private tokens or credentials belong in source control, logs, manifests or PR discussion.

## Repository and change-management policy

- Keep the eight repositories independently buildable and independently versioned.
- Prefer versioned APIs, schemas, SDKs and conformance tests over copying entire codebases.
- Any Git submodule/vendor pin must use an exact immutable commit SHA, record the upstream URL and license evidence, and be reproducible.
- No submodule may point to an excluded repository.
- No source repository history is rewritten.
- No destructive changes or automatic merges are part of this proposal.
- Existing integration PRs must be reconciled against this scope before they are considered compatible.

## Current known integration work and caveats

- MOULI PR #35 currently describes an earlier, incompatible set of submodules (including rustBoot, Memanto and OpenWidgets, and references the OpenWidgets copies of LibreOne/AgentBox). It must not be treated as the implementation of this eight-system scope.
- MOULIONE PR #5 contains a governed inference adapter proposal, but its CI did not execute because GitHub assigned no runner; a live provider/session integration has not been demonstrated.
- LibreOne PR #1 reports passing mocked/API-client CI on Python 3.11–3.13, but no live MOULIONE identity/service integration has been demonstrated.
- AgentBox source documentation identifies a deterministic fake provider and missing production tool sandbox/action gateway; production agent execution is not established by this architecture document.
- TOPSTAR is pinned at `66b5a93f465cdc06a17e8d915a23e45ac75f6c1b` under `integrations/topstar`; its Apache-2.0 license was verified. MOULI now has a bounded C DTO/structural validator and an isolated strict-warning unit test passes. This is not yet a wire decoder, production cryptographic verifier, FFI or runtime integration.
- YNDRA/AetherLang Phase 2 audit is documented in `docs/YNDRA_AETHERLANG_BOUNDARY.md`. YNDRA currently contains semantic IR plus overlapping architecture/physical-compute/OS execution modules; AetherLang documents the hardware compiler pipeline but says its historical source export is absent. Neither source is pinned because explicit license evidence is missing.
- MOULIONE/AgentBox Phase 3 audit is documented in `docs/MOULIONE_AGENTBOX_BOUNDARY.md`. MOULIONE remains enterprise authority; AgentBox owns agent lifecycle/execution/evaluation. AgentBox's fake provider and unwired gVisor runner are not production action execution. Neither repo currently exposes a root license file at the audited commit, so no source pin is added.
- TOMATOAI Phase 5 audit is documented in `docs/TOMATOAI_INTEGRATION.md`. Its VLA interface is proposal-only and the inspected safety helpers are scalar checks, not physical safety certification. No root license evidence is present, so no source pin is added.
- TOMATOAI requires its own build/test and safety-conformance evidence before runtime integration is claimed.
- Added `.github/workflows/eight-system-contracts.yml` to run the full MOULI Makefile test suite and the pinned TOPSTAR Rust workspace tests. The workflow has been committed, but no run result is available yet; this is not counted as passing CI.

## Acceptance criteria for this integration PR

- [ ] Exactly the eight repositories listed above are represented; no extra repository is imported.
- [ ] Exact source commit SHAs and license evidence are recorded for any future source pins.
- [ ] Authority boundaries and versioned contracts are reviewed by each subsystem owner.
- [x] Pin TOPSTAR at an immutable commit and record license evidence.
- [x] Implement the first bounded MOULI-side TOPSTAR DTO/structural validation slice; isolated strict C test passes.
- [ ] Implement and pass TOPSTAR-to-MOULI wire/authorization conformance tests with real cryptographic verification.
- [ ] YNDRA/AetherLang IR and compiler boundary tests pass.
- [ ] MOULIONE/AgentBox tenant authorization and audit integration tests pass.
- [ ] LibreOne live session/chat integration passes against a configured MOULIONE environment.
- [ ] TOMATOAI safety-supervisor and simulated-action tests pass.
- [ ] Explicit approved top-level checkout/build/test succeeds from a clean environment; excluded nested OOPSOne remains uninitialized.
- [ ] Security, dependency, provenance and license reviews are complete.

This PR establishes the architecture and acceptance contract only. It does not claim that all eight codebases have been cloned into MOULI, that runtime integrations are complete, or that the combined system is production-ready.
