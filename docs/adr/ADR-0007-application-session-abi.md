# ADR-0007: Stateful application-session ABI

Status: Accepted, partially implemented

## Decision

Supersede the application-handle portion of ADR-0004 with an additive ABI v2 built around one opaque, per-window application-session handle. Rust owns active-folder and editor consistency, replacement and persistence policy, conflict state, recoveries, pending-native drafts, and identified host effects. Keep the standalone editor and application ABI v1 symbols during migration.

Every valid v2 session call returns one JSON `ApplicationResponse`, including `howler_session_state_json`, whose applied value is `null`. The response contains the authoritative state, effects, and either applied operation data or a structured domain problem. Domain rejection is an ABI success. Invalid handles, malformed input, invalid UTF-8, concurrent use, and serialization failure are transport failures and cannot promise an application snapshot.

Only functions ending in `_json` return `ApplicationResponse`. Returned response and boundary-problem strings are separate Rust allocations exclusively owned by the caller until each is freed exactly once with `howler_session_string_free`. Inputs are borrowed for the call only. Both required output slots are initialized to null before other validation when the slots themselves are valid. A null session is `INVALID_ARGUMENT`; passing a dangling, destroyed, or foreign non-null pointer violates the C caller contract. The session remains caller-owned until one destruction synchronized with every possible use.

V2 `_json` calls return exactly `OK`, `INVALID_ARGUMENT`, `BUSY`, or `INTERNAL`. Stale revision, conflicts, missing notes, and every other domain result are represented inside an `OK` response. `INVALID_ARGUMENT` covers null required pointers, invalid UTF-8, and malformed JSON. `BUSY` means another call owns the non-blocking session lock. `INTERNAL` covers a poisoned lock or response serialization failure.

## Implemented Scope

The implemented session operations cover connection and adoption, note creation/open/close, editing and history, identified saves, independent pending-native-draft persistence and resolution, recovery, conflict resolution, note lifecycle operations, search, reconciliation, and diagnostics. Same-note editor mutation and persistence paths use a shared executor owned by `ApplicationServices`, including across sessions created through the ABI. Until a shared `EditorSession` handle is implemented, the service also owns an active-note lease registry and safely rejects a second open of the same note; two independent revision-zero editors therefore cannot exist.

Pending-native drafts use storage independent from normal recovery. Preserving one cancels the current autosave effect, makes replacement unsafe, and remains unresolved even when independently durable. Normal save and recovery cleanup cannot remove it. Open, rename, move, trash, and adoption paths outside the owning session reject while pending input exists. Resolution first restores the active editor's recovery when needed and removes pending storage durably before publishing safe replacement state.

Pending and conflict save-as-new requests carry a stable `operation_id`. Rust persists the generated destination before canonical creation. Retrying the same operation and source returns the same note, including after canonical commit followed by index, recency, directory-sync, recovery-cleanup, or pending-cleanup failure. Reusing an operation ID with different source is rejected.

All currently exported v2 operations are synchronous and hold the session handle lock for their full execution. Search and diagnostics therefore may cause concurrent typing calls to receive `BUSY`; they do not yet implement the target split-lock query design. This is transport backpressure, not acknowledgement of native input.

## Deferred Scope

Provider file coordination is not implemented. ABI v2 therefore does not expose a host capability table and does not claim coordinated-write safety. Canonical replacement retains the documented validation and in-process serialization guarantees only.

Cancellable rescan/rebuild, external-change event ingestion, event polling, background progress, split-lock immutable queries, and multi-session adoption quiescing remain deferred. Synchronous rescan/rebuild remain available only through the legacy v1/core APIs and are intentionally not exported as v2 session operations.

The Swift package imports the C header and contains typed v2 response/session wrappers, but `AppModel` still uses v1 during migration. Queued `BUSY` retry, guarded native-input replay, native effect execution, and complete AppModel migration remain required before the macOS host can claim the final thin-host architecture.

Milestone 2 task, reminder, notification, and time-zone domains are not part of this ABI until their Rust domain model exists.
