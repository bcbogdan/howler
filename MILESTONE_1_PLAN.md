# Milestone 1 Implementation Plan: Local Notes and Search

Historical host plan: ADR-0008 supersedes the SwiftUI/AppKit implementation details. Rust ownership and compatibility requirements remain applicable.

Status: Draft 1  
Source specification: `SPEC.md`  
Outcome: A usable macOS notes app backed by the reusable Rust editor library

## 1. Objective

Deliver the first complete vertical slice of Howler:

- Open or create a user-owned Markdown note folder.
- Create, edit, rename, delete, and restore notes.
- Present source-preserving rich Markdown through AppKit.
- Persist accepted edits through recovery journals and atomic file saves.
- Detect external changes without knowingly overwriting divergent content.
- Search note titles and bodies.
- Show and hide a floating editor globally.
- Support `Cmd+N` and `Cmd+P` without requiring a network or account.
- Prove that the standalone Rust editor can be embedded independently of Howler storage.

Milestone 1 is complete only when the acceptance criteria in Section 13 pass as an integrated macOS build.

## 2. Non-goals

- Structured task views, deadlines, or reminders.
- Context providers.
- Built-in synchronization.
- Plugin execution.
- Mobile, Linux, or Windows applications.
- A public stable ABI commitment beyond documenting the prototype contract.
- Collaborative editing or CRDTs.
- Automatic merging without a known base version.

## 3. Architecture Delivered

```text
SwiftUI/AppKit macOS host
  -> application C ABI
    -> Rust application services
      -> Rust editor library
      -> note-folder storage
      -> local SQLite index/state
  -> platform brokers
    -> file coordination
    -> global shortcut
    -> native window and permissions
```

The standalone editor C ABI is independently testable:

```text
Test host or future runtime
  -> editor C ABI
    -> Rust editor library
```

The editor library must not depend on filesystem, SQLite, AppKit, or Howler note-folder types.

## 4. Planned Repository Shape

```text
core/
  Cargo.toml
  crates/
    howler-text/
    howler-markdown/
    howler-editor/
    howler-storage/
    howler-search/
    howler-app/
ffi/
  editor/
  application/
apps/
  cli/
  macos/
docs/
  adr/
fixtures/
  editor/
  note-folders/
```

This is a target structure, not permission to create empty crates. Begin with the fewest crates that preserve the editor/application boundary, then split only when dependencies justify it.

## 5. Phase 0: Resolve Blocking Decisions

### 5.1 Goal

Use small executable spikes to settle decisions that would otherwise cause broad rework.

### 5.2 Work

1. Compare candidate rope or text-buffer crates against representative edits.
2. Compare Markdown parsers for byte ranges, malformed input, GFM features, and preservation needs.
3. Prototype the editor transaction and selection model in Rust.
4. Prototype a minimal C ABI call from Swift that creates an editor and applies one edit.
5. Prototype a TextKit adapter with attributed Markdown spans, UTF-8/UTF-16 range conversion, IME, and VoiceOver.
6. Verify the selected macOS minimum supports required TextKit and window APIs.
7. Prototype the global shortcut mechanism, conflict detection, permission behavior, and fallback UX.

### 5.3 Required ADRs

- `ADR-0001`: Rust workspace and editor/application boundaries.
- `ADR-0002`: Text buffer and range coordinate system.
- `ADR-0003`: Markdown parser and source mapping.
- `ADR-0004`: C ABI message encoding and ownership.
- `ADR-0005`: TextKit adapter and minimum macOS version.
- `ADR-0006`: Global shortcut mechanism, default, conflicts, and permissions.

### 5.4 Spike fixtures

- ASCII, emoji, combining marks, and right-to-left text.
- CRLF and LF documents.
- Nested Markdown lists and task syntax.
- Unclosed emphasis, code fences, and malformed front matter.
- 1 MB note with edits near the start, middle, and end.
- IME composition followed by a stale editor revision.

### 5.5 Exit gate

- One Swift test host can edit a Rust-owned document and render returned decorations.
- A committed IME edit reaches Rust without loss or duplicate text.
- UTF-8/UTF-16 conversions round-trip all fixtures.
- Parser operations preserve unrelated source bytes.
- ADRs record selected approaches and rejected alternatives.

No production UI or storage work should depend on unresolved spike APIs.

## 6. Phase 1: Workspace and Quality Baseline

### 6.1 Rust workspace

- Establish the editor library and application-services dependency direction.
- Deny unsafe code by default; isolate required unsafe operations in FFI modules.
- Establish stable error categories and diagnostic structures.
- Add formatting, linting, unit-test, and documentation-test commands.
- Add fixture helpers without coupling production crates to test-only paths.

### 6.2 macOS workspace

- Create the SwiftUI application shell and AppKit adapter target.
- Add Swift tests for range conversion and FFI ownership.
- Keep generated C headers or binding artifacts reproducible.
- Establish debug-only diagnostics without note content in logs.

### 6.3 Continuous integration

At minimum, CI must run:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
Swift/macOS unit tests
C header compilation smoke test
```

macOS UI tests may require a dedicated macOS runner and should be separated from fast core checks.

### 6.4 Exit gate

- A clean checkout builds the Rust workspace, C interfaces, CLI target, and macOS test target.
- CI reports failures at the owning layer.
- Editor crates have no storage or platform dependencies.

## 7. Phase 2: Headless Rust Editor

### 7.1 Text model

Implement:

- UTF-8 source buffer.
- Valid byte-range type.
- Document revision.
- Selection with anchor, head, direction, and affinity.
- Ordered multi-edit transactions.
- Selection transformation through replacements.
- Immutable editor snapshots.

### 7.2 History

Implement deterministic:

- Undo and redo.
- Typing coalescence.
- Paste and formatting boundaries.
- History invalidation after accepted external replacement.
- Advisory host grouping hints.

### 7.3 Markdown semantics

Implement:

- Syntax parsing with source ranges.
- Headings, emphasis, links, code, lists, and checkbox decorations.
- Plain-text projection for search.
- Semantic spans for host accessibility.
- Minimal source edits for formatting commands.
- Unknown syntax preservation.

### 7.4 Public editor API

Implement and test the conceptual operations from `SPEC.md`:

```text
create_editor
apply_edits
execute_editor_command
undo
redo
snapshot
decorations
destroy_editor
```

### 7.5 Tests

- Table tests for edit ordering and overlapping-edit rejection.
- Property tests for arbitrary valid UTF-8 edits.
- Selection-transform tests for insert, delete, replace, and multi-edit cases.
- Deterministic transaction replay tests.
- Undo/redo behavior tests through the public editor API.
- Golden decoration and plain-text fixtures.
- Tests proving unknown Markdown remains byte-identical outside edited ranges.
- Performance checks that guard against full-document copying per keystroke.

Prefer behavioral tests through the public editor API. Test parser internals directly only where public behavior cannot localize failures.

### 7.6 Exit gate

- The editor passes the shared fixture suite without filesystem or SQLite access.
- Replaying the same transactions produces the same snapshot and history.
- Stale revisions reject without mutating source or history.
- A 1 MB note remains within the performance targets established by the spike.

## 8. Phase 3: FFI and AppKit Editor Adapter

### 8.1 Editor C ABI

Implement:

- Opaque editor handles.
- Versioned transaction and result messages.
- Explicit returned-buffer destruction.
- Stable error codes and diagnostic text.
- Serial mutation enforcement.
- Reentrancy and callback-thread rules.

### 8.2 Application C ABI

Before the TextKit adapter is integrated into the product, implement the application C ABI:

- Opaque `NoteFolderHandle` and `NoteEditorHandle` types distinct from `EditorHandle`.
- Note-folder open/create and adoption operations.
- Note create, open, title rename, path move, trash, and restore operations.
- Note-backed edit, command, undo, redo, and save operations.
- Search, rebuild, and diagnostics operations.
- Versioned application events and event polling.
- Returned-buffer ownership, cancellation, executor, callback, and error rules.
- Swift wrappers that prevent mixing standalone and note-backed handles.

The application ABI may initially expose unimplemented operation errors for later phases, but its ownership, threading, event, and versioning contract must be exercised before product code depends on it.

Each downstream phase must implement its application-service operations through this ABI and Swift wrapper before that phase exits; direct Swift calls into Rust internals are prohibited.

### 8.3 TextKit adapter

Implement:

- A native text mirror tagged with Rust revision.
- UTF-8 byte-range to UTF-16 `NSRange` conversion.
- Core decorations mapped to AppKit display attributes.
- Native caret hit testing mapped back to core selections.
- Core-driven formatting commands.
- Core-driven undo and redo.
- Composition state machine from `SPEC.md` Section 11.2.
- AppKit-owned accessibility tree and actions using core semantic spans.

### 8.4 Adapter and application-ABI tests

- Every Unicode range fixture converts in both directions.
- Stale transactions preserve composed input until resolved.
- Composition commit, cancellation, and external replacement are covered.
- AppKit never rewrites Markdown from attributed text.
- Decorations cannot mutate authoritative source.
- VoiceOver can read and navigate representative documents.
- Native undo menu state agrees with Rust history availability.
- Swift cannot pass an `EditorHandle` where a `NoteEditorHandle` is required.
- Application events preserve ordering and release all returned buffers.
- Handle destruction, cancellation, and callback reentrancy follow the documented contract.

### 8.5 Exit gate

- Typing, paste, formatting, undo, redo, dictation, and IME work through Rust transactions.
- Unknown Markdown survives opening, editing another range, and saving.
- The adapter contains no independent Markdown transformation logic.

## 9. Phase 4: Note Folder and Persistence

### 9.1 Folder lifecycle

Implement:

- Create and open note folders.
- Recursive Markdown discovery.
- Ignore `.howler`, `.trash`, and configured ignored paths.
- Provisional folder state keys.
- Optional folder adoption and stable folder identity.
- Provisional note identity and adopted `howler_id` metadata.
- Duplicate-ID diagnostics and repair.
- Path containment, symlink, case, and Unicode-normalization rules.

### 9.2 Note-backed editor handles

Implement `NoteEditorHandle` as an application-service wrapper containing:

- Editor session.
- Note identity and path.
- Base disk hash.
- Recovery generation.
- Current durability state.

All note-backed edits, commands, undo, and redo must pass through this wrapper so accepted revisions schedule recovery and autosave.

Application services maintain one open-session registry entry and one serial operation queue per note identity. The queue orders editor mutations, recovery writes, explicit saves, debounced saves, watcher reconciliation, task-independent note operations, and handle shutdown.

Queue rules:

- Each accepted mutation receives a document revision and recovery generation.
- Persistence jobs carry both values.
- A stale recovery or save completion cannot advance the durability state of a newer revision.
- A newer accepted revision supersedes queued debounced saves but not required recovery persistence.
- Explicit save, note switch, window hide, and graceful termination enqueue a flush barrier.
- Watcher events caused by an in-flight Howler write reconcile after that write completes.
- Destroying a note handle drains accepted work or leaves a durable recovery generation before releasing the session.
- Process termination may interrupt canonical save, but must not discard an already accepted recoverable generation.

### 9.3 Persistence

Implement:

- Device-local application state directory.
- Recovery journals keyed by note and generation.
- Atomic temporary-file replacement.
- Pre-save and coordinated base-hash validation.
- Host file-coordination broker for macOS provider-managed paths.
- Explicit `accepted`, `recovery_durable`, and `file_saved` events.
- Save failure and retry behavior.
- Startup recovery discovery and resolution.

### 9.4 External changes

Implement:

- Recursive file watching.
- Self-generated event suppression followed by reconciliation.
- Rescan after delayed, duplicate, reordered, or coalesced events.
- Clean-session refresh.
- Dirty-session conflict preservation.
- Conflict-copy discovery as a separate provisional note.
- Known-base three-way merge as optional; conflict preservation is mandatory.

### 9.5 Note lifecycle

Implement:

- New note creation.
- Title derivation.
- User-visible rename semantics.
- Move to `.trash`.
- Restore while preserving adopted identity.
- Explicit permanent deletion.

Title rename and file-path rename are separate operations:

- Title rename edits explicit `title` front matter or the heading according to the selected policy, through the editor transaction path.
- Path rename/move changes the file location while preserving adopted note identity, open handle association, index mapping, and watcher reconciliation.
- Neither operation rewrites unrelated front matter or Markdown.

### 9.6 Test seams

Application services should depend on narrow filesystem, watcher, clock, and platform-coordination interfaces. Tests should prefer working temporary-directory implementations and deterministic fake watchers/clocks over interaction-heavy mocks.

### 9.7 Tests

- Interrupted, denied, disk-full, and stale-base saves.
- Simultaneous external writer scenarios.
- Recovery before canonical save and cleanup after successful save.
- Self-generated watcher events and external replacements.
- Rapid revisions with out-of-order recovery/save completions.
- Note switch, hide, quit, and handle destruction with queued writes.
- Folder move before and after adoption.
- Metadata-free notes and refused adoption.
- Duplicate IDs, case collisions, symlinks, and malformed front matter.
- Trash and restore.
- Title rename versus path rename, including open handles and external watcher events.

### 9.8 Exit gate

- A crash after an accepted edit recovers the latest durable draft.
- A detected divergent disk version is not overwritten.
- Existing metadata-free folders remain editable without forced adoption.
- Note lifecycle, note-backed editing, durability events, and conflicts pass through the application ABI integration suite.

## 10. Phase 5: SQLite Index and Search

### 10.1 Index

Implement separate application-local migration streams.

`index.sqlite3` contains disposable derived data:

- Note path, identity, title, timestamps, and content hash.
- Indexing status and stale-index retry.
- FTS5 title and plain-text body.

`state.sqlite3` contains device-local operational data:

- Local recent-note and cursor state.

Deleting or rebuilding `index.sqlite3` must never delete or migrate-reset `state.sqlite3` or recovery journals.

Do not index machine metadata comments or reserved front matter as user search text.

### 10.2 Indexing flow

- Index only stable, completely read files.
- Update after canonical save succeeds.
- Mark stale and retry if indexing fails after a successful file write.
- Reconcile watcher changes by hash.
- Rebuild the index from the note folder without rewriting notes.
- Keep indexing off the editor/UI execution path.

### 10.3 Search

Implement:

- Exact and prefix title ranking.
- Fuzzy title ranking.
- Body FTS matching.
- Bounded recency tie-breaker.
- Deterministic ordering.
- Match snippets suitable for the command palette.
- Cancellation or replacement of obsolete queries.

### 10.4 Diagnostics CLI

Implement CLI commands for:

- Note-folder validation.
- Index rebuild with progress and cancellation.
- Duplicate note ID and malformed metadata reporting.
- Editor, application-service, ABI, and schema version output.
- Redacted diagnostic bundle export.

CLI integration tests use golden note folders and verify that normal diagnostics never emit note content or full paths.

### 10.5 Tests

- Migration tests from every committed schema version.
- Index rebuild equivalence.
- Malformed note isolation.
- Deterministic ranking fixtures.
- Search after external modification and stale-index recovery.
- Search performance over 10,000 representative notes.
- Index deletion preserves recent-note, cursor, window, and recovery state.

### 10.6 Exit gate

- Search meets the p95 target in `SPEC.md` on the reference dataset.
- Deleting `index.sqlite3` and rebuilding restores equivalent results.
- Deleting all application-local databases does not affect Markdown content.
- Search/index work does not block typing.
- CLI validation, rebuild, version, and redacted diagnostic commands pass integration tests.
- Search, index rebuild, progress, cancellation, and diagnostics events pass through the application ABI where used by the macOS host.

## 11. Phase 6: macOS Product Shell

### 11.1 Window and lifecycle

Implement:

- Borderless floating editor panel.
- Pin/always-on-top behavior.
- Configurable global activation shortcut.
- Hide instead of terminate.
- Window geometry and pin-state restoration.
- Multi-display, Spaces, and fullscreen behavior.
- Menu-bar and explicit Quit behavior.

### 11.2 Commands

- `Cmd+N` creates and focuses a note.
- `Cmd+P` overlays search and supports keyboard-only selection.
- `Esc` dismisses transient UI before hiding the panel.
- Standard text commands route through the AppKit adapter and Rust editor.

### 11.3 Palette

- Empty query shows recent notes.
- Results update without blocking editor input.
- Selection opens in the same panel.
- Match reason is visible.
- Cursor position is restored where valid.

### 11.4 Settings

- Note-folder selection.
- Global shortcut configuration.
- Pin and window behavior.
- Diagnostic and index-rebuild actions.

### 11.5 Data safety UI

Implement host/view-model flows for:

- Distinguishing `accepted`, `recovery_durable`, `file_saved`, and failed states without distracting normal typing.
- Retrying or inspecting a failed canonical save.
- Choosing whether to restore a newer recovery draft at startup.
- Resolving a dirty-session external conflict by keeping current, accepting external, comparing, or saving a separate copy.
- Blocking unsafe quit only when neither canonical save nor durable recovery is available.
- Preserving the editor contents while a recovery or conflict decision is shown.

### 11.6 Tests

- Keyboard-only create, edit, search, switch, and hide flow.
- Global activation from another application.
- Window tests across displays, Spaces, and fullscreen apps.
- Permission denial and shortcut conflict behavior.
- App restart restores valid local state without depending on SQLite for note content.
- Startup recovery accept/reject flows.
- External conflict actions preserve both known versions until the user chooses.
- Failed-save retry and durability-state presentation.
- Quit/hide behavior in accepted-only, recovery-durable, file-saved, and failed states.

### 11.7 Exit gate

- The primary workflow can be completed without a mouse.
- The panel appears within the performance target.
- No permanent custom navigation chrome is required for normal editing.

## 12. Phase 7: Hardening and Release Gate

### 12.1 Reliability

- Fault-inject file and database operations.
- Force terminate during typing, recovery write, and canonical save.
- Run external-edit and conflict scenarios repeatedly.
- Confirm one malformed note cannot prevent opening the folder.
- Confirm logs and diagnostics redact content and full paths.

### 12.2 Performance

Measure the targets in `SPEC.md` using committed fixtures:

- Warm panel show.
- Cold editor readiness.
- Recovery and canonical save latency.
- Search over 10,000 notes.
- Opening and editing a 1 MB note.
- Index rebuild progress and cancellation.

### 12.3 Accessibility

- VoiceOver navigation and editing.
- Full keyboard operation.
- Dictation, substitutions, emoji, and representative IMEs.
- Sufficient contrast and visible focus for transient UI.

### 12.4 Packaging

- Signed and notarized direct-download build.
- Clean install and upgrade smoke tests.
- Correct application-support and note-folder permissions.
- No telemetry or note upload by default.

## 13. Milestone Acceptance Checklist

- [ ] A note folder can be created or opened without an account or network.
- [ ] Existing Markdown remains readable and editable outside Howler.
- [ ] Metadata-free folders work without forced adoption.
- [ ] Rust owns authoritative source, transactions, selections, and history.
- [ ] TextKit owns rendering, input, and accessibility without reserializing Markdown.
- [ ] Forced termination during typing recovers the latest durable draft.
- [ ] `Cmd+N` creates and focuses a new note.
- [ ] `Cmd+P` searches title and body with keyboard-only navigation.
- [ ] Unknown Markdown survives unrelated edits.
- [ ] The panel can be globally shown, hidden, moved, resized, and pinned.
- [ ] Index deletion and rebuild restore searchable note content.
- [ ] Delete and restore preserve adopted note identity.
- [ ] Save failures preserve in-memory and recovery copies.
- [ ] Detected external conflicts preserve both known versions.
- [ ] Performance and accessibility gates pass.

## 14. Dependency Order

```text
Blocking ADR spikes
  -> workspace baseline
  -> Rust editor
  -> editor C ABI + TextKit adapter
  -> note-backed application services
  -> persistence and external changes
  -> index and search
  -> macOS shell and palette
  -> hardening and release gate
```

Storage can begin after the editor/application boundary is fixed. Search schema work can proceed in parallel with the TextKit adapter, but integrated search must wait for canonical save and watcher semantics. Product-shell scaffolding may proceed early, but it must not invent a second document model.

## 15. Completion Artifacts

- Rust editor and application crates.
- Separate editor and application C interfaces.
- Swift/AppKit editor adapter.
- macOS application implementing the primary workflow.
- CLI commands for validation and index rebuild.
- ADRs listed in Phase 0.
- Editor and note-folder fixture suites.
- Signed release candidate and acceptance report.
