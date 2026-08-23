# Howler

Milestone 1 local-first Markdown editor and note-folder services.

## Requirements

- Stable Rust 1.80 or newer
- A C compiler for ABI header smoke checks
- macOS 13 and Xcode/Swift 5.10 for the native app

## Rust

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

The standalone `howler-editor` crate has no filesystem or database dependencies. `howler-app` stores canonical content only in Markdown. Pass a device-local state root when opening a folder; its disposable FTS5 index, `state.sqlite3` recency/cursor state, and recovery drafts are created under `folders/<folder-state-id>`.

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

Headers are in `ffi/editor/include` and `ffi/application/include`. Handles and returned strings must be released with the matching API's destroy/free function. JSON messages use ABI version 1 semantics; calls are synchronous and handles must be mutated serially.

## macOS

Build the matching Rust static library first, then build the app on macOS: `cargo build -p howler-application-ffi`, `swift test --package-path apps/macos`, and `swift build --package-path apps/macos`. Release builds require `cargo build --release -p howler-application-ffi` before `swift build -c release --package-path apps/macos`. SwiftPM cannot run Cargo as a package build step; `Package.swift` selects the debug or release archive by Swift configuration. No dynamic library needs to be embedded. The Swift package contains the floating AppKit panel, SwiftUI shell, application-FFI wrappers, TextKit adapter, range/JSON-contract tests, global shortcut, file-backed create/edit/save, recovery chooser, and search palette.
