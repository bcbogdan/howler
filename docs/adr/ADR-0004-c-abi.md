# ADR-0004: C ABI messages and ownership

Status: Accepted

Expose distinct opaque editor, folder, and note-editor handles. Complex input and output use UTF-8 JSON; the exported ABI version query identifies the prototype function and message contract, while payloads have no separate version envelope. No Rust layout crosses the boundary. Rust allocates returned NUL-terminated strings and each ABI provides its matching free function. Stable integer result categories cover invalid input, stale revisions, conflicts, ownership, duplicate identity, existing destinations, busy handles, I/O, and internal failures. Calls are synchronous. Handles use non-blocking mutex acquisition and return `BUSY` on concurrent use; note editors retain their owning folder-context identity.
