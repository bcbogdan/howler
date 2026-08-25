# Howler

Milestone 1 local-first Markdown editor and note-folder services.

## Requirements

- Stable Rust 1.80 or newer
- A C compiler for ABI header smoke checks
- Zig 0.16.0 and macOS 13 for the native app

## Rust

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

The standalone `howler-editor` crate has no filesystem or database dependencies. `howler-app` stores canonical content only in Markdown. Pass a device-local state root when opening a folder; its disposable FTS5 index, `state.sqlite3` recency/cursor state, recovery drafts, and independent pending-native drafts are created under `folders/<folder-state-id>`. Pending-native files are not normal autosave recovery and survive until explicit Rust-owned resolution.

## CLI

```sh
cargo run -p howler-cli --bin howler -- versions
cargo run -p howler-cli --bin howler -- validate <folder> --state <state-directory>
cargo run -p howler-cli --bin howler -- rebuild <folder> --state <state-directory>
cargo run -p howler-cli --bin howler -- rescan <folder> --state <state-directory>
cargo run -p howler-cli --bin howler -- recoveries <folder> --state <state-directory>
cargo run -p howler-cli --bin howler -- bundle <folder> --state <state-directory>
cargo run -p howler-cli --bin howler -- search <folder> <query> --state <state-directory>
```

Diagnostics print relative paths, stable codes, and no note bodies or absolute note paths.

Saves validate the base hash before preparing a same-directory temporary file and again immediately before replacement while holding Howler's per-note operation lock. This serializes in-process application operations, but it is not a cross-process filesystem compare-and-swap: an external process can still race the final validation and rename. Recovery is retained on conflicts and canonical-write failures, and save results separately report recovery cleanup and index freshness.

Mutation paths reject symlink components and revalidate immediately before use. Descriptor-relative `openat` traversal is not implemented, so an external process with write access to the folder can still race path-component replacement between validation and mutation.

## C ABI

Headers are in `ffi/editor/include` and `ffi/application/include`. The folder/editor API remains ABI v1. The partially implemented application-session ABI v2 returns authoritative state, identified effects, and structured domain outcomes; domain rejection still returns transport status `OK`. Session response and boundary strings are caller-owned and must be released with `howler_session_string_free`; v1 strings use `howler_application_string_free`. Callers must synchronize destruction with all use.

V2 calls are currently synchronous, hold the non-blocking session lock for the full operation, and return `BUSY` rather than wait. Search and diagnostics have not yet been moved to split-lock immutable query connections. Cancellable rescan/rebuild and event polling are not exported through v2. Provider-coordinated writes are also deferred, so v2 exposes no native capability table and canonical saves provide in-process per-note serialization and second-hash validation, not a filesystem compare-and-swap. See `docs/adr/ADR-0007-application-session-abi.md` for implemented and deferred scope.

The public v2 JSON operation matrix is `docs/APPLICATION_SESSION_V2_JSON.md`; common state, response, problem, effect, and request structures are checked in at `docs/schema/application-session-v2.schema.json`.

## Native app

The Native SDK migration host is under `apps/native`. Build the Rust application ABI with `cargo build -p howler-application-ffi`, then run `zig build` or `zig build test` from `apps/native` using Zig 0.16.0. The SDK dependency temporarily points at the local fork checkout until commit `e6a9b595` is pushed to an accessible remote; after that it must be replaced by an exact URL and package hash. macOS packaging and runtime acceptance remain migration gates.
