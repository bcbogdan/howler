# Native SDK Migration Plan

## Decision

Replace the SwiftUI/AppKit macOS host with a Native SDK host written in Zig.

- Target macOS first. Add Windows and Linux after macOS parity.
- Use Native SDK's editable Markdown code component. Do not retain TextKit or add platform editor adapters.
- Maintain a pinned Native SDK fork for required editor and macOS capabilities.
- Delete the Swift/AppKit host when migration implementation begins. Git history remains the implementation reference; there will be no parallel production host.
- Retain the Rust editor, application services, application-session ABI, note format, recovery format, and application-state storage.
- Keep Rust authoritative for source, revisions, history, persistence, conflicts, recovery, replacement safety, and autosave identity.
- Keep Zig responsible for presentation, native input, windows, dialogs, timers, accessibility, and platform capabilities.

## Target Repository Shape

```text
apps/native/
  app.json
  build.zig
  build.zig.zon
  assets/
  src/
    main.zig
    app.native
    model.zig
    session.zig
    session_types.zig
    editor_bridge.zig
    effects.zig
    platform.zig
    tests.zig
  tests/
    fixtures/
    automation/
```

`model.zig` contains presentation state and the latest authoritative Rust state. It must not reproduce persistence, conflict, recovery, or replacement policy.

## Phase 1: Reset the Host Architecture

1. Tag the final pre-migration repository state for reference.
2. Convert reusable Swift JSON and range test cases into language-neutral fixtures.
3. Delete `apps/macos` and remove SwiftPM-specific build configuration.
4. Scaffold `apps/native` with `native init --template zig-core --full`.
5. Add an ADR that supersedes ADR-0005 and records the Native SDK boundary.
6. Update `SPEC.md`, `THIN_NATIVE_HOST.md`, the milestone plans, and README build instructions.
7. Preserve bundle ID `app.howler.mac`, the `Howler` Application Support directory, note-folder formats, recovery files, pending-native-draft files, and Rust database schemas.

Exit criteria:

- The repository has one production UI host.
- Rust and CLI tests still pass after deleting the Swift host.
- Existing note and application-state fixtures are preserved for compatibility testing.

## Phase 2: Pin and Extend Native SDK

Fork Native SDK from a released version and pin an exact commit and Zig package hash. Do not track the fork's default branch from Howler.

Required fork capabilities:

1. Controlled editor source and selection properties.
2. An externally supplied editor revision token.
3. A mode that disables engine-owned undo and routes undo and redo to application commands.
4. Input-origin metadata for typing, paste, composition, dictation, autocorrection, and isolated replacements where the platform can identify them.
5. A close and hide request that the application can approve, reject, or defer while it persists input.
6. System-wide global shortcut registration.
7. macOS all-Spaces and full-screen auxiliary window behavior.
8. Configurable minimum macOS version.
9. At least 1 MiB of editable UTF-8 source without truncation.
10. Tests proving that controlled source and selection updates during IME composition do not lose or duplicate committed text.

Keep these changes narrow and upstreamable. Each fork change requires a focused SDK-level test.

Exit criteria:

- The controlled Markdown editor passes source, selection, undo, IME, close-veto, and 1 MiB tests.
- A global shortcut can show the window while another application is active.
- The window works across macOS Spaces and beside full-screen applications.
- No Howler note folder has been opened by the new host yet.

## Phase 3: Prepare the Rust ABI

Keep application-session ABI v2 and extend it additively.

1. Add a selection-only operation that updates anchor, head, affinity, and cursor state without changing source revision or creating history.
2. Add optional input-origin metadata to text-edit requests.
3. Add explicit capability reporting so the host can fail closed when required operations are unavailable.
4. Update the C header, JSON schema, public operation matrix, and ABI tests.
5. Keep response ownership unchanged: every returned Rust string is freed exactly once with `howler_session_string_free`.
6. Keep ABI v1 until confirmed external consumers and compatibility policy permit removal.

Required tests:

- Forward and reversed selections.
- Invalid UTF-8 boundaries.
- Stale selection revisions.
- Selection-only changes do not alter document revision or history.
- Capability negotiation.
- Invalid JSON and UTF-8 handling.
- Response and boundary-buffer ownership.

Exit criteria:

- Rust workspace and ABI tests pass.
- Existing ABI v2 operations retain their current wire contracts.

## Phase 4: Build the Zig Session Layer

Implement `session.zig` using `@cImport` of `ffi/application/include/howler_application.h`.

The session layer must:

- Own exactly one application-session handle.
- Run calls through one serialized worker so synchronous Rust work never blocks the Native SDK UI loop.
- Prioritize committed input over search, diagnostics, and background presentation work.
- Join the worker before destroying the session handle.
- Distinguish transport failure from domain rejection.
- Decode required fields and enums strictly while tolerating unknown optional object fields.
- Free every non-null Rust response and boundary string exactly once.
- Retain exact request data across `BUSY`.
- Return operation results to the Native SDK update loop as typed messages.
- Avoid direct note-folder mutation outside Rust application services.

Exit criteria:

- Zig tests reproduce the existing Swift contract and ownership coverage.
- Real ABI integration tests connect disposable folders and exercise every operation used by the host.
- Session shutdown cannot overlap an active C call.

## Phase 5: Implement the Editor Bridge

Use Native SDK's editable `<code language="markdown">` component.

Maintain three explicit values:

1. The authoritative Rust snapshot.
2. The Native SDK controlled mirror, including active composition.
3. Optional committed input awaiting Rust acknowledgement.

Input flow:

1. Receive a Native SDK text event.
2. Apply the event to the controlled mirror.
3. Submit selection-only events through the selection ABI.
4. Convert committed source changes into UTF-8 replacements against the expected Rust revision.
5. Keep marked composition host-local until commit.
6. On `BUSY`, stop admitting input based on that revision and retry the exact payload.
7. On domain rejection, install the returned authoritative state first.
8. Replay stale input only when the original text, ranges, and adjacent context still identify the same edit location.
9. If replay is unsafe, preserve the complete native source through `preserve_pending_native_draft` and retain the local copy until Rust reports it durable.
10. Block editor replacement, hide, close, and quit while committed input exists only in Zig memory.
11. Route undo, redo, formatting, and commands to Rust, then install the returned source and selection.
12. Apply decorations only when their revision matches the installed snapshot.

Exit criteria:

- Unicode, emoji, combining marks, RTL text, and reversed selections round-trip correctly.
- IME commit and cancellation do not duplicate or lose text.
- Forced `BUSY`, stale revision, persistence failure, hide, and quit scenarios retain all committed input.
- Native SDK and Rust never maintain competing undo histories.

## Phase 6: Rebuild the Product UI

Implement in `app.native`:

- Empty-folder onboarding and a native folder dialog.
- Editable Markdown surface.
- Search palette and keyboard result selection.
- Recovery chooser.
- Pending-native-draft preservation and resolution.
- Conflict comparison and resolution.
- Save and durability status.
- Settings for folder, shortcut, and window behavior.
- Rename, move, trash, and restore operations.
- Diagnostics and background-task presentation when the Rust APIs exist.

Command behavior:

- Command-N creates a note and focuses the editor.
- Command-P opens and focuses the palette.
- Command-Z and Command-Shift-Z invoke Rust history.
- Escape dismisses the topmost transient surface, then requests a safe hide.
- Command-Option-H globally toggles the panel.
- Quit waits for Rust replacement safety and refuses unsafe termination.

Map Rust `HostEffect` values to keyed Native SDK timers. Timer callbacks must retain the exact `SaveTarget`; they must not reconstruct one from current state.

Exit criteria:

- The new host covers all workflows previously exposed by the Swift host.
- No Zig code derives durability or replacement policy from diagnostic strings.
- No Zig code writes canonical notes, recovery files, or application databases directly.

## Phase 7: Testing and Automation

### Zig Headless Tests

- ApplicationResponse decoding and required-enum rejection.
- Unknown optional fields.
- Model transitions and modal priority.
- Text-event reduction and selection mapping.
- Autosave scheduling, cancellation, and stale timers.
- Exact `BUSY` retry.
- Pending-draft handoff.
- Layout sweeps and accessibility audits.

### Rust ABI Integration Tests

Use temporary note and state directories for:

- Create, edit, save, and reopen.
- Forced termination and recovery.
- Recovery restore and discard.
- Pending-native-draft restart and resolution.
- Clean external refresh.
- Dirty external conflict.
- Save-as-new idempotency.
- Recovery-write and canonical-write failures.
- Parent-sync uncertainty.
- Stale index and rebuild.
- Symlink and path-escape rejection.
- Duplicate identities.

### Native SDK Automation

- Global summon while another app is active.
- Input immediately after show.
- Command-N focus.
- Command-P keyboard navigation.
- Undo and redo.
- Hide and restore.
- Modal dismissal order.
- Window resize and geometry restoration.
- Deterministic screenshots and accessibility snapshots.

### Manual macOS Matrix

- VoiceOver.
- Japanese, Chinese, and Korean IMEs.
- Dead keys, dictation, substitutions, autocorrection, and emoji.
- Multiple displays and display rearrangement.
- Spaces and full-screen applications.
- Shortcut collisions.
- Denied folder access and File Provider folders.
- Sleep and wake.
- Quit during composition and save.
- Gatekeeper, clean install, and upgrade.

## Phase 8: Package and Release

Build order:

```text
pinned Native SDK fork
  -> Rust application static library
  -> Zig host
  -> Native SDK package
  -> sign
  -> notarize
  -> staple
```

Release requirements:

1. Preserve bundle ID `app.howler.mac` and display name `Howler`.
2. Preserve the existing Application Support path and state formats.
3. Build arm64 first.
4. Add x86_64 and universal packaging after macOS parity.
5. Set and verify the intended minimum macOS version.
6. Produce a signed and notarized `.app` and DMG.
7. Verify installation and launch from `/Applications` on a clean account.
8. Prevent concurrent Howler processes from operating on the same application state and folder.

## Release Gates

### Framework Gate

- Controlled selection and source updates work.
- Rust owns undo and redo.
- Close and hide can be deferred safely.
- Global activation works.
- IME and 1 MiB documents pass.
- Required macOS window behavior passes.

### Data-Safety Gate

- No committed input can be lost through `BUSY`, stale revisions, composition, hide, quit, or persistence failure.
- Existing notes, recovery files, and pending drafts open without migration.
- Every returned Rust allocation is freed exactly once.

### Product Gate

- Create, edit, reopen, recovery, external-change, conflict, lifecycle, Unicode, IME, shortcut, panel, focus, and VoiceOver scenarios pass.
- Keyboard-only operation is complete.

### Distribution Gate

- The signed and notarized application launches from `/Applications`.
- A clean install and upgrade preserve all user data.
- Automation and manual smoke tests pass against the packaged artifact.

## Follow-Up Work

After the replacement host reaches parity:

1. Add provider coordination through the Native SDK macOS platform layer and the planned Rust coordinated-commit continuation.
2. Implement asynchronous rescan, rebuild, event polling, and watcher ingestion.
3. Add revision-matched semantic decoration spans to the Native SDK editor.
4. Add Windows and Linux targets using the same Zig host.
5. Add task, reminder, and notification workflows.

## Critical Path

```text
Native SDK fork capabilities
  -> additive Rust ABI work
  -> Zig session wrapper
  -> safe editor bridge
  -> product UI parity
  -> automation and manual tests
  -> signed macOS release
```
