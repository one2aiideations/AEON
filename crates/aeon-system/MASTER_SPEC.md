# AEON-SYSTEM — Engineering Specification

## Product objective
A single native Rust desktop application that serves as a unified, AI-native control centre for operating, understanding, maintaining, securing and optimizing Windows, Linux and macOS computers.

## Non-negotiable architecture
- One executable; one Rust source file at `src/main.rs`.
- Internal Rust modules may be declared inside that file.
- No frontend/backend split, microservices, sidecars, Python, Node, Java, Electron or Flutter runtime.
- Native OS adapters and native utilities are permitted.
- Local/offline core; AEON and AEON-DB integrations remain optional. AEONONE is the enterprise identity/authorization authority only when enterprise integration is enabled.
- Never silently overwrite existing working code.

## Product areas
1. Home dashboard and transparent system-health summary.
2. System information and hardware inventory.
3. Process and resource manager.
4. Services and startup manager.
5. Storage, disks and file-system utilities.
6. Network diagnostics and configuration.
7. Device and driver inventory.
8. Applications and native package managers.
9. Cleanup and optimization recommendations.
10. Security posture and native protection integrations.
11. Event and log centre.
12. Power and battery.
13. Backup and recovery.
14. OS and application updates.
15. Users, groups and sessions.
16. Scheduled tasks and automation.
17. Developer and advanced tools.
18. AI assistant with internal specialist agents.
19. Reporting, audit and observability.

## Safety contract
- Read-only default and least privilege.
- Explicit elevation only when required.
- Every mutation requires validated inputs, dry-run preview, user approval, risk classification, audit record and post-action verification.
- Provide rollback where feasible; otherwise state clearly that rollback is unavailable.
- No arbitrary AI-generated shell execution, shell-string interpolation, credential collection, covert monitoring or silent disabling of antivirus, firewall, encryption or OS protections.
- Protect critical processes, services, directories and user data.
- Deletion must enumerate exact targets and obtain confirmation.
- Unsupported capabilities must be marked unavailable.

## Current implementation boundary
The first increment is a real read-only inventory and dashboard using sysinfo. It includes host identity, uptime, CPU/memory, processes, disks and network counters, plus local JSON report export and audit logging. The UI must not imply that unimplemented modules are operational.

## Delivery stages
1. Inspect repository and build environment.
2. Establish Rust monolith and desktop UI.
3. Detect host OS and capabilities.
4. Implement live dashboard and system inventory.
5. Add process, service and startup management with safe action gates.
6. Add storage and file utilities.
7. Add network and device management.
8. Add applications and package manager adapters.
9. Add security and event centre.
10. Add maintenance, backup and recovery.
11. Add AI assistant and bounded internal agents.
12. Add automation and reporting.
13. Validate each supported OS.
14. Run security, performance and production-readiness audits.

## Verification discipline
For each subsystem report separately: implemented, compiled, unit tested, integration tested, verified on real OS, production ready. Never infer runtime correctness from UI presence. Tests must use mock providers and must not mutate the host OS.

## Initial acceptance criteria
- `cargo check` succeeds on supported toolchains.
- App launches on each claimed platform.
- Live inventory refreshes and gracefully handles unavailable fields.
- Search filters process inventory.
- JSON report export produces valid JSON.
- Audit log records report export and refresh.
- No system-changing operation is exposed in the read-only foundation.
