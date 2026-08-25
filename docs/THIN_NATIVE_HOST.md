# Thin Native Host Architecture

Implementation status: this document describes the target architecture. ADR-0007 records the
currently implemented session ABI and explicitly deferred work. In particular, provider write
coordination, split-lock queries, and cancellable background rescan/rebuild are not implemented or
exported through session ABI v2 yet; the capability-table and asynchronous APIs below remain design
requirements rather than current safety guarantees.

## Principle

Rust owns application policy and state transitions. Native SDK and Zig own native mechanisms and presentation.

The host must not independently coordinate folder, editor, durability, recovery, conflict, or replacement invariants. Those invariants belong in Rust, where they can be tested on every supported development platform. Native SDK remains responsible for behavior that is inherently native.

The boundary has two Rust layers:

- `ApplicationServices` owns folders, indexing, recovery, the open-note registry, and serial per-note mutation executors. It may serve multiple windows and future background features.
- `ApplicationSession` is a per-window controller. It tracks the folder and active editor presented by that window and delegates mutations to `ApplicationServices`.

`ApplicationSession` is not a global lock around all application work. Search, indexing, rescan, and unrelated note operations must not block typing. Mutations of the same note must use the same per-note executor whether they originate from an editor, autosave, reconciliation, a task view, or a notification.

## Rust Responsibilities

Add a stateful session above the existing `NoteFolder` and `NoteEditor` behavior:

```rust
pub struct ApplicationSession {
    services: Arc<ApplicationServices>,
    folder: Option<FolderContext>,
    active: Option<ActiveEditor>,
    recoveries: Vec<RecoveryDraft>,
    generation: u64,
}

pub struct ActiveEditor {
    note_id: Identity,
    editor: NoteEditorHandle,
    persistence: PersistenceState,
    conflict: Option<ConflictState>,
    pending_native_draft: Option<PendingNativeDraft>,
}
```

Keeping identity, editor, persistence, and conflict state in one value prevents mismatched combinations such as an editor for one note and a separate current-note ID for another.

The application-facing API must cover the existing workflows rather than only editing:

```rust
impl ApplicationSession {
    pub fn connect(&mut self, request: ConnectFolder) -> ApplicationResponse<ConnectResult>;
    pub fn adopt_folder(&mut self) -> ApplicationResponse<ConnectResult>;
    pub fn create_note(&mut self, request: CreateNote) -> ApplicationResponse<NoteResult>;
    pub fn open_note(&mut self, id: &str) -> ApplicationResponse<NoteResult>;
    pub fn close_note(&mut self) -> ApplicationResponse<()>;

    pub fn apply_text_edit(&mut self, edit: HostTextEdit) -> ApplicationResponse<EditResult>;
    pub fn preserve_pending_native_draft(&mut self, draft: PendingNativeDraft) -> ApplicationResponse<()>;
    pub fn resolve_pending_native_draft(&mut self, resolution: PendingDraftResolution) -> ApplicationResponse<NoteResult>;
    pub fn execute_command(&mut self, command: HostEditorCommand) -> ApplicationResponse<EditResult>;
    pub fn undo(&mut self, expected_revision: u64) -> ApplicationResponse<Option<EditResult>>;
    pub fn redo(&mut self, expected_revision: u64) -> ApplicationResponse<Option<EditResult>>;
    pub fn save(&mut self, target: SaveTarget) -> ApplicationResponse<SaveResult>;
    pub fn resolve_conflict(&mut self, resolution: ConflictResolution) -> ApplicationResponse<NoteResult>;

    pub fn restore_recovery(&mut self, id: &str) -> ApplicationResponse<NoteResult>;
    pub fn discard_recovery(&mut self, id: &str) -> ApplicationResponse<()>;

    pub fn search(&self, query: SearchQuery) -> ApplicationResponse<SearchResults>;
    pub fn rename_note(&mut self, request: RenameNote) -> ApplicationResponse<NoteResult>;
    pub fn move_note(&mut self, request: MoveNote) -> ApplicationResponse<NoteResult>;
    pub fn trash_note(&mut self, id: &str) -> ApplicationResponse<TrashResult>;
    pub fn restore_note(&mut self, request: RestoreNote) -> ApplicationResponse<NoteResult>;

    pub fn list_tasks(&self, query: TaskQuery) -> ApplicationResponse<TaskPage>;
    pub fn set_open_task_completed(&mut self, request: OpenTaskMutation) -> ApplicationResponse<EditResult>;
    pub fn set_indexed_task_completed(&mut self, request: IndexedTaskMutation) -> ApplicationResponse<NoteResult>;
    pub fn set_task_deadline(&mut self, request: TaskDeadlineMutation) -> ApplicationResponse<TaskResult>;
    pub fn set_task_reminder(&mut self, request: TaskReminderMutation) -> ApplicationResponse<TaskResult>;
    pub fn report_notification_result(&mut self, result: NotificationResult) -> ApplicationResponse<()>;
    pub fn update_time_zone(&mut self, change: TimeZoneChange) -> ApplicationResponse<TaskStarted>;
    pub fn update_notification_authorization(&mut self, state: NotificationAuthorization) -> ApplicationResponse<TaskStarted>;

    pub fn start_rescan(&mut self) -> ApplicationResponse<TaskStarted>;
    pub fn start_rebuild(&mut self) -> ApplicationResponse<TaskStarted>;
    pub fn notify_external_changes(&mut self, changes: Vec<ExternalChange>) -> ApplicationResponse<TaskStarted>;
    pub fn cancel_background_task(&mut self, id: BackgroundTaskId) -> ApplicationResponse<()>;
    pub fn poll_events(&mut self) -> ApplicationResponse<Vec<ApplicationEvent>>;
    pub fn diagnostics(&self) -> ApplicationResponse<Vec<Diagnostic>>;
    pub fn diagnostic_bundle(&self) -> ApplicationResponse<DiagnosticBundle>;
}
```

`ConnectFolder` includes the note-folder path, application-state path, adoption choice, and whether a missing folder may be created. The Native SDK host discovers native paths and obtains user consent; Rust validates and applies the request.

Long-running rescan and rebuild work is asynchronous, cancellable, and reports progress through application events. Search may remain synchronous while it meets its latency target, but it must use an immutable/index connection path that does not take an active editor's mutation lock.

Open-note task mutations carry the note identity and expected document revision and use its existing executor. Closed-note task mutations carry the indexed content hash, re-read and verify the canonical file, and create at most one transient editor under the same per-note executor. Deadline and reminder requests use the same routing. Notification scheduling remains native: Rust emits scheduling intents and consumes structured success or failure results through `report_notification_result`.

Reminder creation for a provisional folder is rejected with `AdoptionRequired`; the host can obtain consent and call `adopt_folder`, then retry the unchanged reminder request. Time-zone and notification-authorization changes enter through explicit operations that asynchronously recompute overdue state and reconcile notification intents.

Adoption first requires every active editor in the folder to reach a safe recovery boundary. Application services quiesce the folder's note executors, rewrite metadata, and atomically remap registry entries, active-editor identities, recovery records, and indexed identities before publishing the adopted folder generation. Existing autosave targets are cancelled; sessions receive new identities and generations in their next state. Any failure leaves the provisional mapping authoritative and its recoveries intact.

Native file watchers submit paths through `notify_external_changes`; Rust coalesces events, suppresses its own writes, and routes reconciliation through the affected note executors. A full rescan uses the same reconciliation path.

Rust owns:

- Save-before-open, create, close, connect, or adoption policy.
- Whether the current editor can safely be replaced.
- Active note and editor consistency.
- Recovery discovery and resolution.
- Durability transitions and retry state.
- Conflict versus clean-refresh decisions.
- Per-note mutation serialization.
- Transaction validation, construction, and revision handling after Zig supplies native edit facts.
- The authoritative state returned after each operation.

## Operation Responses

A valid call against a usable session returns current state even when the requested operation is rejected:

```rust
pub struct ApplicationResponse<T> {
    pub state: ApplicationState,
    pub effects: Vec<HostEffect>,
    pub outcome: OperationOutcome<T>,
}

pub enum OperationOutcome<T> {
    Applied(T),
    Rejected(ApplicationProblem),
}

pub struct ApplicationProblem {
    pub code: ProblemCode,
    pub diagnostic: String,
    pub details: Option<ProblemDetails>,
}

pub enum ProblemCode {
    NotConnected,
    NoteNotFound,
    RecoveryNotFound,
    RecoveryPending,
    StaleRevision,
    ExternalConflict,
    IdentityChanged,
    StaleEditor,
    WrongOwner,
    DestinationExists,
    DuplicateIdentity,
    InvalidOperation,
    PersistenceFailure,
    TaskNotFound,
    ContentHashMismatch,
    AdoptionRequired,
    DatabaseFailure,
}

pub enum ProblemDetails {
    StaleRevision {
        expected_revision: u64,
        current_revision: u64,
    },
    ExternalConflict {
        external_source: String,
        external_hash: String,
    },
    RecoveryPending {
        note_id: Identity,
    },
    Persistence {
        issues: Vec<PersistenceIssue>,
    },
    ContentHashMismatch {
        expected_hash: String,
        current_hash: String,
    },
    AdoptionRequired {
        folder_path: String,
    },
}
```

`ProblemCode` is stable and machine-readable. `diagnostic` is for logs and fallback presentation, not policy or localization. Zig chooses localized presentation from the code and structured details. Each code defines which detail variant is required; schema tests reject a missing or mismatched required detail. New optional fields may be ignored, while new codes or detail variants require an ABI schema version understood by the host.

Current `AppError` and `EditorError` cases map once at the application boundary. Missing notes and recoveries, pending recovery, stale revision, external conflict, identity or generation changes, ownership, destination, duplicate identity, input/domain validation, persistence, content-hash, adoption, and database failures map to their corresponding codes above. The mapping must not discard fields needed for retry or conflict resolution.

Stale revisions include the current revision. External conflicts include the external source and content hash while the authoritative state retains the local editor source and recovery status. A rejected rescan or save therefore does not lose either side.

Conflict resolution is explicit:

```rust
pub enum ConflictResolution {
    UseExternal { expected_external_hash: String },
    KeepLocalAsNewNote {
        operation_id: String,
        expected_external_hash: String,
        title: Option<String>,
    },
}
```

`UseExternal` requires an explicit user action and removes the local recovery only after the external source is installed successfully. `KeepLocalAsNewNote` preserves the local source as a new note before refreshing the original note. A changed external hash rejects the resolution with updated conflict details.

Failures that happen before a usable session can be identified, such as a null handle, concurrent handle use, invalid UTF-8, or malformed JSON, are ABI boundary failures and cannot promise an application snapshot.

## Authoritative State

State returned to Zig contains everything needed to render the active editor coherently:

```rust
pub struct ApplicationState {
    pub folder: Option<FolderState>,
    pub active: Option<ActiveEditorState>,
    pub recoveries: Vec<RecoveryDraft>,
    pub background_tasks: Vec<BackgroundTaskState>,
}

pub struct ActiveEditorState {
    pub note_id: Identity,
    pub editor: EditorPresentationState,
    pub persistence: PersistenceState,
    pub conflict: Option<ConflictState>,
    pub pending_native_draft: Option<PendingNativeDraftState>,
    pub generation: u64,
}

pub struct EditorPresentationState {
    pub snapshot: EditorSnapshot,
    pub decorations: DecorationSet,
}

pub struct DecorationSet {
    pub revision: u64,
    pub items: Vec<Decoration>,
}
```

`DecorationSet.revision` must equal `EditorSnapshot.revision`. Open, edit, command, undo, redo, conflict resolution, and external refresh all return a coherent source, selection, revision, history state, and decorations. If decoration payload size later requires range queries, those queries must require an expected revision and may not silently combine results from different revisions.

Persistence retains independent outcomes instead of reducing them to one issue:

```rust
pub struct PersistenceState {
    pub durability: DurabilityState,
    pub replacement_safety: ReplacementSafety,
    pub issues: Vec<PersistenceIssue>,
}

pub enum ReplacementSafety {
    Safe,
    MustRetainEditor,
}

pub enum PersistenceIssue {
    RecoveryWrite { diagnostic: String },
    CanonicalWrite { diagnostic: String },
    CanonicalDurabilityUncertain { diagnostic: String },
    RecoveryCleanup { diagnostic: String },
    IndexStale { diagnostic: String },
    RecencyUpdate { diagnostic: String },
}
```

Rust derives replacement safety from the complete persistence state. In particular, a canonical replacement followed by parent-directory sync failure remains dirty, retains recovery, reports canonical durability uncertainty, and is safe to replace only when recovery is known durable.

## Native Edit Contract

The Zig Native SDK host reports native input facts; Rust constructs and applies the application transaction:

```rust
pub struct HostTextEdit {
    pub expected_revision: u64,
    pub replacements: Vec<Replacement>,
    pub selections: Vec<Selection>,
    pub history: HistoryHint,
    pub composition: Option<CompositionCommit>,
}

pub struct CompositionCommit {
    pub original_range: TextRange,
    pub original_text: String,
}
```

Replacement ranges use the source at `expected_revision`. Supplied selections use post-edit UTF-8 coordinates and carry `expected_revision + 1`; Rust validates them against the resulting source. An empty selection list asks Rust to transform the previous selections. Selections preserve anchor, head, affinity, and revision. The initial macOS host may expose one selection, but it must not collapse a reversed selection to its upper bound.

The host retains a committed IME payload after a stale-revision rejection. It refreshes from the returned state and replays only when `original_range` still contains `original_text`; carrying the text avoids a cross-language hash contract. Rust never reads platform marked-text state directly.

`BUSY` is transport backpressure, not rejection of committed input. The host queues the exact edit or composition payload together with its pre-edit snapshot and retries it before accepting another native transaction based on that Rust revision. A `BUSY` result never causes native input to be discarded or treated as acknowledged.

If the retry becomes stale, the host refreshes authoritative state and replays only when each original range, replaced text, and adjacent context still identify the same edit location. Otherwise it submits the resulting native source and base revision through `preserve_pending_native_draft` and retains its local copy until Rust acknowledges durable recovery. Rust then exposes the pending draft in `ApplicationState`, sets replacement safety, and offers explicit save-as-new or discard resolution. This guarded stale-rebase protocol applies to ordinary committed input as well as IME commits; unreplayable input never remains solely in presentation state.

## Native Host Responsibilities

Keep inherently native behavior in the Zig Native SDK host and its platform backend:

- Native text rendering and attributed decorations.
- Platform text-range to UTF-8 range conversion.
- Caret, selection, scrolling, and focus.
- IME marked-text composition and safe replay of a rejected commit.
- VoiceOver and accessibility actions.
- `NSPanel`, Spaces, displays, and fullscreen behavior.
- Global shortcut registration.
- Folder selection and Application Support path discovery.
- Native file watching, provider permission prompts, and platform file coordination.
- Local notification scheduling and delivery.
- Native SDK presentation and localized messages.
- Timers and other platform effects requested by Rust.

The host decides how a problem is presented. Rust decides what the problem means, which state transition occurred, and whether replacing the current editor is safe.

Palette visibility, query text, focus, and localized strings remain presentation state. Search results are operation data consumed by the palette. Durability, conflict, current-editor invariants, and replacement safety do not remain in Zig.

## Update And Effect Flow

Effects carry identity so delayed native work cannot act on a different note:

```rust
pub enum HostEffect {
    ScheduleAutosave {
        effect_id: EffectId,
        delay_ms: u64,
        target: SaveTarget,
    },
    CancelEffect {
        effect_id: EffectId,
    },
    ScheduleNotification {
        intent: NotificationIntent,
    },
    CancelNotification {
        intent_id: NotificationIntentId,
    },
}

pub struct SaveTarget {
    pub note_id: Identity,
    pub revision: u64,
    pub generation: u64,
}
```

When a timer fires, Zig calls `save(target)`. Rust rejects a target whose identity, revision, or generation is no longer current. Opening, closing, or connecting increments the generation and cancels superseded effects.

Notification effects contain stable intent IDs. The host reports the result of every scheduling or cancellation attempt, allowing Rust to retain retry state and diagnostics rather than assuming a native effect succeeded.

Platform file coordination is a synchronous native capability injected into `ApplicationServices`, not a post-operation effect. On provider-managed macOS locations, the capability enters a Rust commit continuation inside the native coordination accessor. The continuation performs the coordinated re-read, second hash validation, atomic replacement, and parent synchronization. The callback runs on the calling operation's executor and must not reenter an application-session ABI function. Other platforms provide their strongest equivalent advisory mechanism or an explicitly unsupported implementation.

The editing flow becomes:

```text
Native text control commits input
  -> Zig converts ranges and selections to UTF-8
  -> ApplicationSession routes the edit through the note executor
  -> Rust accepts the transaction and persists or schedules recovery
  -> Rust returns authoritative state, operation data, and identified effects
  -> Zig updates attributes and only replaces native text when it differs
  -> Zig executes the effects
```

The host must not call `apply`, separately request a snapshot, then derive durability or replacement policy itself.

## Thin Presentation Model

The Zig model primarily consumes responses and executes native effects. Its shape is intentionally small:

```zig
pub const AppModel = struct {
    state: ApplicationState,
    palette_open: bool = false,
    palette_query: []const u8 = "",

    pub fn consume(self: *AppModel, response: ApplicationResponse) void {
        self.state = response.state;
        self.effects.perform(response.effects);
        if (response.problem) |problem| self.present(problem);
    }
};
```

The Zig decoding layer uses explicit enums and tagged unions for known Rust values. It must preserve or safely reject unknown required values according to the ABI schema contract; importing a C header alone does not type JSON fields.

## C ABI

Expose an opaque application-session handle through explicit operations. The session API is an application ABI v2 because it changes the handle and response contracts accepted by ADR-0004 and ABI v1.

**Future target snippet, not the current exported header:** the capability table and coordinated
write signatures below are design requirements. The current partial v2 header intentionally omits
them until the continuation is wired into canonical replacement. Consumers must compile against
`ffi/application/include/howler_application.h`, not copy declarations from this target example.

Before implementation, add an ADR that supersedes the application-handle portion of ADR-0004. The standalone editor ABI remains separate. Keep ABI v1 symbols during the macOS migration and for any confirmed external consumers; remove them only under the repository's major-version compatibility policy.

```c
typedef struct HowlerApplicationSession HowlerApplicationSession;

typedef int32_t (*HowlerCoordinatedOperation)(void *operation_context);
typedef int32_t (*HowlerCoordinateWrite)(
    void *host_context,
    const char *path,
    void *operation_context,
    HowlerCoordinatedOperation operation
);

typedef struct HowlerHostCapabilitiesV2 {
    size_t struct_size;
    void *host_context;
    HowlerCoordinateWrite coordinate_write;
} HowlerHostCapabilitiesV2;

uint32_t howler_session_abi_version(void);

int32_t howler_session_create(
    const HowlerHostCapabilitiesV2 *capabilities,
    HowlerApplicationSession **out_session
);

void howler_session_destroy(
    HowlerApplicationSession *session
);

int32_t howler_session_apply_text_edit_json(
    HowlerApplicationSession *session,
    const char *edit_json,
    char **out_response_json,
    char **out_boundary_problem_json
);

int32_t howler_session_save_json(
    HowlerApplicationSession *session,
    const char *target_json,
    char **out_response_json,
    char **out_boundary_problem_json
);

void howler_session_string_free(char *string);
```

Every session operation has an explicit C function even though complex values use JSON. This provides operation-specific documentation, typed Zig declarations, and small test surfaces without a generic dispatcher.

The capability table and its context remain valid until session destruction. `struct_size` permits compatible optional additions. After coordination is established, `coordinate_write` invokes the supplied Rust operation exactly once inside the native coordination accessor and returns its status. If coordination cannot be established, it invokes the operation zero times and returns the distinct coordination-unavailable status. It must never invoke the operation more than once, retain the operation context, or reenter a session function. A null callback also means platform coordination is unavailable, which Rust may accept for ordinary local folders but rejects when the selected location requires provider coordination.

For a valid call on a locked session, `out_response_json` contains `ApplicationResponse`, including rejected domain operations. `out_boundary_problem_json` is reserved for failures where coherent session state cannot be returned. Both output pointers are initialized to null before other validation. The ABI defines nullability, status categories, string ownership, handle destruction synchronization, and non-blocking `BUSY` behavior.

Translate the checked-in C header with Zig rather than manually redeclaring symbols. The C compiler and Zig then verify the call contract. JSON schemas and Zig response types remain a separate, versioned contract with unknown-field tests.

The C handle lock protects session state only. `ApplicationServices` releases it before long-running work and uses per-note executors and background task handles. No rescan or rebuild runs synchronously while holding the session lock.

Session transitions linearize when their final `ApplicationState` is published. A query briefly clones the folder-service reference and folder generation, releases the session lock while querying, then reacquires it to publish a response. If the folder generation changed, the query is rejected as stale instead of combining results with another connection. The final state capture is coherent at that linearization point. Lock acquisition is non-blocking; a losing concurrent call returns `BUSY` and may be retried, so search and background completion never wait behind typing. Native input follows the mandatory queued retry protocol above.

## Test Strategy

### Rust Editor Tests

Continue testing editor behavior through `EditorSession`:

- UTF-8 boundaries and range validation.
- Forward and reversed selection transforms.
- Typing coalescence and history boundaries.
- Undo and redo.
- Stale revisions.
- Markdown decorations and source preservation.

### Rust Application Tests

Test workflows through `ApplicationSession` and shared `ApplicationServices`:

- Connecting an empty folder creates and opens a note.
- Pending recovery prevents unsafe opening of that note without blocking unrelated notes.
- Restore and discard transitions are deterministic.
- Opening another note forces save policy and respects replacement safety.
- Accepted-only text prevents unsafe editor replacement.
- Recovery-durable text permits replacement.
- Canonical-save failures preserve recovery and all independent issues.
- Parent-directory sync failure reports canonical uncertainty and derives safety from recovery durability.
- Rescan refreshes a clean editor.
- Rescan returns local and external sides of a dirty conflict.
- Conflict resolution checks the external hash and never silently discards local text.
- Every accepted mutation returns its matching snapshot and decorations.
- Every domain rejection returns current state and structured problem details.
- Stale autosave targets cannot save a newly opened note.
- Unreplayable native input becomes a Rust-owned durable pending draft before replacement is allowed.
- Unrelated note mutation and search do not take the active note executor.
- Same-note editor, autosave, reconciliation, and task mutations serialize through one executor.
- Provisional-folder reminders return `AdoptionRequired` and succeed after explicit adoption.
- Time-zone and notification-authorization changes reconcile reminder state and intents.
- Notification scheduling failures remain visible and retryable.
- Index deletion and rebuild preserve operational state and search.
- Unknown Markdown survives complete application workflows.

Inject failures at narrow persistence phases rather than replacing the whole commit with one binary seam. Required phases include recovery write, temporary write, file sync, second validation, atomic replacement, parent sync, recovery cleanup, index transaction, and recency update. Production uses the real implementations; tests use deterministic phase failures.

### Application ABI Tests

Test:

- Session ownership, destruction, and destruction synchronization.
- Output pointer initialization and nullability.
- Concurrent-use `BUSY` behavior.
- Capability-table size, lifetime, null coordination, callback ordering, and reentrancy rejection.
- ABI version reporting.
- Response and boundary-problem buffer ownership.
- Invalid UTF-8 and JSON handling.
- Applied and rejected response contracts.
- Unknown optional fields and incompatible required enum values.
- Structured stale-revision and conflict details.
- C header compilation and Zig translation.
- ABI v1 and v2 symbol coexistence during migration.

### Native Host Tests

Use fixture responses and the real ABI integration seam for the Zig model. Do not mock every C function independently.

Test native boundaries:

- UTF-8 and UTF-16 range conversion.
- Forward and reversed selections.
- IME commit, cancellation, stale-revision refresh, and guarded replay.
- Queued retry of committed input after `BUSY`, including durable handoff when stale replay is unsafe.
- Revision-matched decoration-to-native-text attribute mapping.
- VoiceOver semantics and actions.
- Editor focus after `Cmd+N`.
- Keyboard-only palette navigation.
- Panel show, hide, move, resize, and pin behavior.
- Identified effect scheduling, cancellation, and stale timer firing.
- Rust response decoding, unknown values, and C response memory ownership.
- Localized presentation of structured problems without policy string comparisons.

Keep a small integrated macOS suite for create/edit/reopen, forced-termination recovery, external refresh and conflict resolution, VoiceOver smoke testing, and global panel activation.

## Migration Sequence

1. Add characterization tests for current save, recovery, replacement, reconciliation, selection, decoration, and conflict behavior.
2. Record the v2 session ABI decision in an ADR that supersedes the application-handle portion of ADR-0004.
3. Add the shared open-note registry and serial per-note executor to application services.
4. Add `ApplicationSession` to `howler-app`, delegating to existing folder and editor types rather than rewriting storage logic.
5. Introduce `ApplicationResponse`, authoritative editor presentation state, structured problems, and identified effects.
6. Move apply, commands, undo, redo, and coherent snapshot/decorations into the session.
7. Move save, persistence issues, replacement safety, and stale autosave validation into the session.
8. Move open, create, close, connect, recovery, and conflict-resolution transitions into the session.
9. Route search, note lifecycle, tasks, reminders, and notification results through the facade without taking unrelated note executors.
10. Add the host capability table and coordinated-commit continuation before moving canonical save calls to ABI v2.
11. Move rescan and rebuild to cancellable application tasks and event polling.
12. Expose explicit session operations through ABI v2 while retaining ABI v1 during migration.
13. Translate the C header in Zig and add versioned response types with unknown-value handling.
14. Keep the Zig model limited to response consumption, presentation state, queued input retry, and native effects.
15. Remove superseded ABI v1 operations only when compatibility policy and known consumers permit it.
16. Complete native text decoration, accessibility, focus, selection, and IME tests.

This boundary keeps correctness and data-safety workflows testable through Rust on Linux and macOS, preserves per-note concurrency for future application features, and reserves the smaller set of genuinely platform-specific acceptance tests for macOS.
