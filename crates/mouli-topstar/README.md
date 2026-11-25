# mouli-topstar

TOPSTAR core domain model for MOULI, imported from `topstar-core` (Apache-2.0, same author).

Capabilities, responsibility contracts, authorization and revocation, devices and device registry/discovery,
the wire protocol envelope, sessions, an append-only journal, execution records with a durable store,
audit log and an event broker.

Changes from upstream (the upstream snapshot did not compile or pass its own tests):

- `NetworkAddress`, `Endpoint`, `Transport`, `NetworkIdentity`, `SessionState`, `EventRecord` derive serde.
- `chat::MessageId` is exported as `ChatMessageId` (it clashed with `protocol::MessageId`); network types re-exported.
- `ProtocolError::Serialization` holds a `String` (`serde_json::Error` is not `Eq`).
- `Journal::open` returns `InvalidHeader` for files shorter than the header.
- `Policy` implements `Default` (empty policy, permits nothing).
- Repaired stale tests (executor fixtures, record-store assertions, protocol version test); one corrupted test line fixed.
- Edition 2021 and the workspace lint set are not applied yet; the crate keeps its own `Cargo.toml`.

`mouli-adapters::topstar` still talks to the C-era contract; wiring it to this crate is the next step.
