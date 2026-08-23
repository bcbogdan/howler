# Thin Native Host Architecture

## Principle

Rust owns application policy and state transitions. Swift owns native mechanisms and presentation.

The macOS host should not independently coordinate folder, editor, durability, recovery, and conflict invariants. Those invariants belong in Rust, where they can be tested on every supported development platform. AppKit remains responsible for behavior that is inherently native.

## Rust Responsibilities

Add a stateful `ApplicationSession` above the existing `NoteFolder` and `NoteEditor` types:

```rust
pub struct ApplicationSession {
    folder: Option<NoteFolder>,
    editor: Option<NoteEditor>,
    current_note_id: Option<Identity>,
    durability: DurabilityState,
    recoveries: Vec<RecoveryDraft>,
}
```

The facade should expose explicit operations:

```rust
impl ApplicationSession {
    pub fn connect(&mut self, request: ConnectFolder) -> Result<ApplicationUpdate, AppError>;
    pub fn create_note(&mut self) -> Result<ApplicationUpdate, AppError>;
    pub fn open_note(&mut self, id: &str) -> Result<ApplicationUpdate, AppError>;
    pub fn apply_text_edit(&mut self, edit: HostTextEdit) -> Result<ApplicationUpdate, AppError>;
    pub fn undo(&mut self) -> Result<ApplicationUpdate, AppError>;
    pub fn redo(&mut self) -> Result<ApplicationUpdate, AppError>;
    pub fn save(&mut self) -> Result<ApplicationUpdate, AppError>;
    pub fn restore_recovery(&mut self, id: &str) -> Result<ApplicationUpdate, AppError>;
    pub fn discard_recovery(&mut self, id: &str) -> Result<ApplicationUpdate, AppError>;
    pub fn rescan(&mut self) -> Result<ApplicationUpdate, AppError>;
}
```

Rust owns:

- Save-before-open, create, or connect policy.
- Whether the current editor can safely be replaced.
- Current note and editor consistency.
- Recovery discovery and resolution.
- Durability transitions.
- Conflict versus clean-refresh decisions.
- Transaction construction and revision handling.
- The authoritative snapshot returned after each operation.

Swift must not reconstruct policy from string comparisons such as `durability == "recovery_durable"`. Rust should return typed policy results:

```rust
pub enum ReplacementSafety {
    Safe,
    MustRetainEditor,
}

pub struct PersistenceState {
    pub durability: DurabilityState,
    pub replacement_safety: ReplacementSafety,
    pub issue: Option<PersistenceIssue>,
}
```

## Swift Responsibilities

Keep inherently native behavior in Swift and AppKit:

- `NSTextView` rendering and attributed decorations.
- UTF-16 `NSRange` to UTF-8 range conversion.
- Caret, selection, scrolling, and focus.
- IME marked-text composition.
- VoiceOver and accessibility actions.
- `NSPanel`, Spaces, displays, and fullscreen behavior.
- Global shortcut registration.
- Folder selection and Application Support path discovery.
- SwiftUI presentation and localized messages.
- Timers and other platform effects requested by Rust.

Swift decides how an error is presented. Rust decides what the error means and whether replacing the current editor is safe.

## Update And Effect Flow

A Rust operation should return one coherent update:

```rust
pub struct ApplicationUpdate {
    pub state: ApplicationState,
    pub effects: Vec<HostEffect>,
}

pub struct ApplicationState {
    pub editor: Option<EditorSnapshot>,
    pub current_note_id: Option<Identity>,
    pub recoveries: Vec<RecoveryDraft>,
    pub persistence: PersistenceState,
}

pub enum HostEffect {
    ScheduleAutosave { delay_ms: u64 },
    CancelAutosave,
}
```

The editing flow becomes:

```text
TextKit commits native input
  -> Swift converts the range to UTF-8
  -> ApplicationSession applies the transaction
  -> Rust persists recovery and updates session state
  -> Rust returns the authoritative snapshot and effects
  -> Swift displays the snapshot
  -> Swift executes the autosave scheduling effect
```

Swift should not call `apply`, separately request a snapshot, then derive durability policy itself.

## Thin AppModel

The final Swift model should primarily consume updates and execute native effects:

```swift
@MainActor
final class AppModel: ObservableObject {
    @Published var state = ApplicationState.empty
    @Published var palettePresented = false
    @Published var paletteQuery = ""

    private let session: RustApplicationSession
    private let effects: EffectScheduler

    func apply(_ edit: NativeTextEdit) {
        do {
            consume(try session.applyTextEdit(edit))
        } catch {
            present(error)
        }
    }

    private func consume(_ update: ApplicationUpdate) {
        state = update.state
        effects.perform(update.effects)
    }
}
```

Palette visibility, queries, and localized strings remain presentation state. Durability, current-editor invariants, and replacement safety do not.

## C ABI

Expose an opaque application-session handle through explicit operations:

```c
typedef struct HowlerApplicationSession HowlerApplicationSession;

int32_t howler_session_create(
    HowlerApplicationSession **out_session
);

int32_t howler_session_apply_text_edit_json(
    HowlerApplicationSession *session,
    const char *edit_json,
    char **out_update_json,
    char **out_problem_json
);

int32_t howler_session_save(
    HowlerApplicationSession *session,
    char **out_update_json,
    char **out_problem_json
);
```

Prefer explicit functions over one generic JSON dispatcher. Explicit operations provide clearer documentation, operation-specific types, better Swift discoverability, and smaller test surfaces.

Import the C header through a SwiftPM C target or module map instead of manually redeclaring every symbol with `@_silgen_name`. This lets Clang verify that Swift calls match the public C contract.

## Test Strategy

### Rust Editor Tests

Continue testing editor behavior through `EditorSession`:

- UTF-8 boundaries and range validation.
- Selection transforms.
- Typing coalescence and history boundaries.
- Undo and redo.
- Stale revisions.
- Markdown decorations and source preservation.

### Rust Application Tests

Test workflows through `ApplicationSession`:

- Connecting an empty folder creates and opens a note.
- Pending recovery prevents unsafe opening.
- Restore and discard transitions are deterministic.
- Opening another note forces a safe save.
- Accepted-only text prevents unsafe editor replacement.
- Recovery-durable text permits replacement.
- Canonical-save failures preserve recovery.
- Rescan refreshes a clean editor.
- Rescan preserves both sides of a dirty conflict.
- Every accepted mutation returns its matching authoritative snapshot.
- Index deletion and rebuild preserve operational state and search.
- Unknown Markdown survives complete application workflows.

Introduce narrow dependency seams only where fault injection requires them. For example, abstract the canonical commit boundary rather than the entire filesystem:

```rust
trait FileCommitter {
    fn commit(&self, path: &Path, contents: &[u8]) -> Result<(), AppError>;
}
```

Production uses the real atomic writer. Tests use deterministic denied-write, disk-full, and interrupted-write implementations.

### Application ABI Tests

Test:

- Session ownership and destruction.
- Output pointer initialization.
- Concurrent-use `BUSY` behavior.
- ABI version reporting.
- Result and problem buffer ownership.
- Invalid UTF-8 and JSON handling.
- Structured update and problem contracts.
- C header compilation.

### Swift Tests

Use a fake `ApplicationSessionProtocol` as the seam for `AppModel`. Do not mock every C function independently.

Test native boundaries:

- UTF-8 and UTF-16 range conversion.
- Forward and reversed selections.
- IME commit, cancellation, and stale-revision behavior.
- Decoration-to-TextKit attribute mapping.
- VoiceOver semantics and actions.
- Editor focus after `Cmd+N`.
- Keyboard-only palette navigation.
- Panel show, hide, move, resize, and pin behavior.
- Rust result decoding and memory ownership.

Keep a small integrated macOS suite for create/edit/reopen, forced-termination recovery, external refresh and conflict, VoiceOver smoke testing, and global panel activation.

## Migration Sequence

1. Add characterization tests for current save, recovery, replacement, and reconciliation behavior.
2. Add `ApplicationSession` to `howler-app`, delegating to existing types rather than rewriting storage logic.
3. Move apply, undo, redo, and authoritative snapshot results into the session.
4. Move save and replacement-safety policy into the session.
5. Move open, create, connect, and recovery transitions into the session.
6. Move rescan and reconciliation coordination into the session.
7. Expose session operations through the application C ABI.
8. Import the C header as a Swift module and replace raw string states with Swift enums.
9. Reduce `AppModel` to update consumption, presentation state, and native effects.
10. Remove superseded folder/editor ABI operations after Swift no longer uses them.
11. Complete TextKit decoration, accessibility, focus, and IME tests.

This boundary keeps correctness and data-safety workflows testable through Rust on Linux and macOS while reserving the smaller set of genuinely AppKit-specific acceptance tests for macOS.
