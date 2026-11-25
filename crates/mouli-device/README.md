# mouli-device

The controller that connects the on-device screens (`mouli-screen`) to the governed workspace
(`mouli-interface`) and to code review (`mouli-code`).

- Chat goes through `Workspace::submit` (policy, routing, washing, audit); the model sits behind the `Backend` trait.
- Approvals are decided with the four-eyes rule: nobody approves their own request.
- The Audit and Security screens are rebuilt from the real hash chain and approval queue.
- A proposed diff is assessed against the real files and applied or undone atomically; every step is audited. The controller checks policy itself and does not rely on the screen.

21 integration tests. Not yet linked into `moulid`; that waits for the OS build window.
- `attach_apps` connects the Sheet and Studio applications (`mouli-apps`): edits made on their screens run as governed capability calls, so they are policy-checked, audited, and (for erasing) need a second person's approval.
