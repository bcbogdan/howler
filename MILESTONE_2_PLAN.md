# Milestone 2 Implementation Plan: Structured Tasks

Historical host plan: ADR-0008 supersedes the SwiftUI/AppKit implementation details. Rust ownership and product requirements remain applicable.

Status: Draft 1  
Source specification: `SPEC.md`  
Dependency: Milestone 1 acceptance criteria pass

## 1. Objective

Add structured task management without moving task truth out of Markdown:

- Extract standard Markdown checkboxes.
- Give tasks stable identity only when required.
- Group tasks by note and heading path.
- Support explicit deadlines and timezone-aware reminders.
- Show, filter, and complete tasks outside the note editor.
- Keep note-editor and task-view updates in one Rust transaction model.
- Schedule and cancel local macOS notifications through the host.
- Rebuild all canonical task state from Markdown after index deletion.

Milestone 2 is complete only when task state, deadlines, and reminders survive index deletion and all acceptance criteria in Section 11 pass.

## 2. Preconditions

Milestone 1 must provide:

- Stable editor transactions and command API.
- `NoteEditorHandle` with revision and disk-hash concurrency.
- Atomic saves and external-conflict handling.
- Adopted and provisional note identity behavior.
- SQLite migrations and rebuild infrastructure.
- Native command routing and local application state.
- AppKit editor decorations, checkbox parsing, and checkbox source ranges. Task interaction commands are delivered by this milestone.

Task work must not create an alternate persistence or edit path around these components.

## 3. Non-goals

- Recurring tasks.
- Cross-note dependency graphs.
- Project-management workflows.
- Multi-user assignment.
- Natural-language deadline inference that causes side effects.
- Remote push notifications.
- Built-in device synchronization.
- Calendar synchronization.
- Plugin-defined task storage.

## 4. Phase 0: Task Syntax ADR

### 4.1 Goal

Freeze a minimal, readable, source-preserving task annotation syntax before schema or UI work.

### 4.2 Decisions

The ADR must define:

- Stable task ID syntax.
- Deadline syntax.
- Reminder syntax.
- Offset and named-time-zone representation.
- Daylight-saving behavior.
- Invalid and duplicate annotation handling.
- Annotation placement on nested and multiline list items.
- Copy/paste behavior for stable IDs.
- When an ordinary checkbox receives an ID.
- Whether completed tasks retain deadline/reminder annotations.

Candidate source remains conceptually:

```markdown
- [ ] Send agenda @due(2026-08-24) @remind(2026-08-24T09:00:00-07:00)
  <!-- howler-task-id: 01J6Y4B4N8F9A2C7D1E5G3H6K0 -->
```

This example is not final until the ADR is accepted.

### 4.3 Required properties

- Valid ordinary Markdown.
- Human-readable deadline and reminder data.
- Machine-only identity does not pollute visible task text.
- Unknown or malformed annotations remain byte-preserved.
- Parsing does not depend on locale or natural-language guessing.
- The syntax can round-trip through common Markdown editors.
- A reminder identifies an unambiguous instant and preserves intended zone behavior.

### 4.4 Fixtures

- Simple, nested, and multiline tasks.
- Blockquotes and ordered lists.
- Duplicate copied task IDs.
- Missing, malformed, repeated, and conflicting annotations.
- Leap days and daylight-saving gaps/overlaps.
- Completed tasks with active reminder annotations.
- Annotation-like text inside code spans and code fences.

### 4.5 Exit gate

- ADR accepted.
- Parser spike returns exact ranges for every fixture.
- Source-edit spike can add, update, and remove annotations without rewriting task text.

## 5. Phase 1: Task Domain and Extraction

### 5.1 Domain model

Implement:

```text
Task
  id: stable or provisional task identity
  note_id: stable or provisional note identity
  source_range
  source_content_hash
  text
  completed
  heading_path
  deadline
  reminder
  annotation_diagnostics
```

A provisional task identity is derived from note identity, source range, and content hash. It is not persisted or used for durable external state.

### 5.2 Extraction

Extend the pure Markdown/editor layer to extract:

- Checkbox marker and completion state.
- User-visible task text excluding machine metadata.
- Containing heading path.
- Stable ID annotation when present.
- Deadline and reminder annotations.
- Exact ranges for checkbox and annotation operations.
- Diagnostics without modifying invalid source.

### 5.3 Stable identity

Application services own ID allocation. They request an editor command to insert a supplied ID when a task first gains durable external state, including:

- Reminder scheduling.

Merely indexing a checkbox must not rewrite the note.

A date-only deadline does not require stable identity and must not insert an ID unless the task syntax ADR establishes a concrete current requirement.

### 5.4 Tests

- Golden extraction fixtures.
- Property tests around arbitrary surrounding Markdown.
- Tests that code blocks and escaped checkboxes are ignored.
- Heading-path tests under edits and nested sections.
- Duplicate-ID diagnostics.
- Tests proving invalid annotations remain unchanged.

### 5.5 Exit gate

- Every valid fixture yields deterministic tasks and ranges.
- Metadata-free checkboxes remain source-identical after indexing.
- Parser failures in one task do not suppress other tasks in the note.

## 6. Phase 2: Task Editor Commands

### 6.1 Commands

Add editor-library commands for:

- Toggle completion.
- Set or clear deadline.
- Set or clear reminder.
- Insert a supplied stable task ID.
- Apply ID plus reminder/deadline changes atomically.

Commands operate against expected document revision and task source range or stable ID. They produce ordinary `EditResult` values and participate in undo/redo.

The combined command inserts an ID only when the requested operation requires stable identity, such as scheduling a reminder. Setting a deadline alone does not insert one.

### 6.2 Application ABI extensions

Extend the application C ABI and Swift wrappers with:

- Paged task queries and filter messages.
- Open-note task mutation using `NoteEditorHandle` and document revision.
- Closed-note task mutation using task identity and indexed content hash.
- Deadline and reminder commands.
- Provisional task-update events distinct from canonical commit events.
- Canonical task-index and reminder-intent events.
- Notification scheduling-result commands.
- Versioned task, deadline, reminder, and diagnostic payloads.

Add ownership, cancellation, event ordering, unknown-field, and ABI compatibility tests for every new message family.

### 6.3 Command invariants

- Replace the smallest necessary source range.
- Never rewrite unrelated list formatting.
- Preserve line endings and indentation.
- Reject stale revisions without mutation.
- Reject ambiguous duplicate stable IDs.
- Keep deadline and reminder changes in one undo group when initiated by one user action.
- Completing a task cancels its scheduled notification but does not silently delete its source annotation unless the ADR explicitly requires it.

### 6.4 Per-note mutation routing

Application services retain the Milestone 1 open-session registry and serial per-note executor. Every editor, task-view, notification, and external-change mutation resolves note identity through this registry before selecting a path.

- If a note is open, all mutations use its existing `NoteEditorHandle` and queue.
- If it is closed, application services create at most one transient session under the same per-note queue.
- A second closed-note action waits for or joins that session; it cannot create a competing writer.
- Notification and task-view actions serialize with typing, autosave, watcher reconciliation, and each other.
- Closing an editor does not expose a closed-note path until accepted persistence work reaches a safe recovery boundary.

### 6.5 Open-note path

```text
Host
  -> application services with NoteEditorHandle + expected revision
  -> editor command
  -> accepted transaction
  -> recovery/autosave
  -> canonical save
  -> committed index refresh
  -> canonical reminder event
```

### 6.6 Closed-note path

```text
Host
  -> application services with task ID + indexed content hash
  -> re-read and verify file hash
  -> transient editor session
  -> editor command
  -> optimistic-concurrency save
  -> committed index refresh
  -> canonical reminder event
```

A hash mismatch rejects the action and triggers reindexing. It does not apply an old source range to new content.

### 6.7 Tests

- Commands through the public editor API.
- Undo and redo for every task operation.
- Open-note stale-revision rejection.
- Closed-note stale-content-hash rejection.
- External write during task save.
- Checkbox edits with unusual indentation and line endings.
- Combined ID/reminder transaction atomicity.
- Task-view action racing editor typing.
- Notification completion racing an open editor edit.
- Two simultaneous closed-note actions.
- Task mutation racing watcher reconciliation.
- Accepted task edit followed by canonical save failure.
- Canonical save success followed by task reindex failure and retry.

### 6.8 Exit gate

- Open and closed note paths produce equivalent canonical Markdown.
- Every task action is undoable when its editor session remains open.
- No task mutation bypasses editor commands or optimistic file concurrency.
- No note has more than one active mutation session or executor.

## 7. Phase 3: Task Index and Rebuild

### 7.1 Schema

Add a derived `tasks` table containing:

- Stable ID when present.
- Provisional indexed identity.
- Note identity.
- Source range and source content hash.
- Search/display text.
- Completion state.
- Heading path.
- Deadline date.
- Reminder instant and zone metadata.
- Parse diagnostics.

The table is disposable. Canonical fields must be reconstructible from Markdown.

Device-local notification identifiers may live in application state, keyed by stable folder identity plus a validated unique task ID. They are operational mappings, not task truth. Reminder scheduling is suppressed while a task ID is duplicated or ambiguous.

### 7.2 Index lifecycle

- Re-extract tasks after successful canonical note saves.
- Re-extract after accepted external file changes.
- Remove task rows when notes are trashed or removed.
- Rebuild all tasks from the note folder.
- Reconcile notification intents after startup and rebuild.
- Treat source ranges as invalid whenever their content hash differs.
- Emit reminder scheduling or cancellation intent only after canonical file save and task reindex commit succeed.

### 7.3 Query API

Support filters required by the first task view:

- Open versus completed.
- Due today.
- Overdue.
- Upcoming deadline.
- Note.
- Heading group.

Ordering should be deterministic, with overdue and upcoming deadlines ahead of undated tasks unless the user selects another filter.

### 7.4 Tests

- Migration and rollback-safety tests.
- Index deletion and full rebuild.
- Rebuild equivalence against task fixtures.
- Note deletion, restore, rename, and external modification.
- Duplicate stable ID diagnostics.
- Duplicate ID introduction and removal during reminder reconciliation.
- Query ordering across time zones and local-date boundaries.

Use an injectable clock in application services so deadline tests do not depend on wall time.

### 7.5 Exit gate

- Deleting the task/index database and rebuilding restores all canonical task fields.
- No stale source range is used after a content-hash change.
- Queries remain responsive on the Milestone 1 reference note set with representative task density.

## 8. Phase 4: Task View and Editor Integration

### 8.1 Task view

Implement a transient task surface that follows the minimal macOS design:

- No permanent sidebar.
- Keyboard-first filtering and navigation.
- Grouping by note and heading path.
- Open/completed, due, overdue, and upcoming filters.
- Completion action.
- Open source note at the task location.
- Deadline and reminder editing.

### 8.2 Editor integration

- Render checkboxes using editor-library semantic decorations.
- Clicking or invoking a checkbox executes a Rust task command.
- Reveal enough source syntax when directly editing annotations.
- Update the task view after accepted editor transactions and committed index changes.
- Preserve selection and scroll position after task source edits.

### 8.3 Consistency rules

- The editor session is authoritative while a note is open.
- Task view actions against an open note use its current revision, not the stale index range.
- The index may lag a successful file save briefly but must visibly converge.
- A conflict or save failure remains visible; the task view must not optimistically report durable completion indefinitely.
- Accepted task transactions may update provisional UI, but notification scheduling and cancellation wait for canonical save and committed task reindexing.

### 8.4 Tests

- Keyboard-only task filtering, completion, and note opening.
- Checkbox click updates Markdown and aggregate view.
- Aggregate completion updates an open editor.
- Stale index action refreshes rather than touching the wrong source range.
- Undo after task-view completion updates both editor and task view.
- Accessibility labels expose task text, completion, deadline, and reminder state.
- Canonical save failure rolls provisional task UI back or marks it unsaved without changing notification state.

### 8.5 Exit gate

- Completing a task from either surface produces the same source edit.
- UI state agrees with accepted, recovery-durable, file-saved, and failed states.
- A task action cannot transfer a reminder to a neighboring task after source movement.

## 9. Phase 5: Deadlines and Reminders

### 9.1 Deadline semantics

- Deadlines are local calendar dates, not implicit midnight instants.
- Overdue evaluation uses the user's current calendar and time zone.
- Changing device time zone recomputes views without changing source dates.
- Invalid dates remain in source and produce diagnostics.

### 9.2 Reminder semantics

- Reminder annotations resolve to explicit instants while retaining source zone intent.
- Application services derive desired notification state from canonical Markdown.
- Reminder creation is unavailable until the folder and note have stable adopted identities. The host offers adoption and returns a typed provisional-identity error if the user declines.
- The macOS host owns notification authorization and platform identifiers.
- Application services never remove source reminders because permission was denied.
- Completing or deleting a task emits cancellation intent only after canonical commit.
- Editing a reminder replaces its previous scheduled intent only after canonical commit.

### 9.3 Host protocol

```text
Application services, after file save + task-index commit
  -> desired_reminder(folder_id, task_id, instant, content)
Host
  -> request permission only when first needed
  -> schedule or cancel native notification
  -> reminder_schedule_result
Application services
  -> record device-local operational mapping/status by folder + unique task ID
```

Notification actions may:

- Open the correct note and task.
- Complete the task through the normal open/closed task command path.

### 9.4 Reconciliation

On startup, note-folder switch, time-zone change, index rebuild, or notification authorization change:

1. Derive desired reminders from indexed canonical task data.
2. Exclude provisional notes and duplicate or ambiguous task IDs.
3. Compare with device-local scheduled mappings.
4. Schedule missing future reminders.
5. Cancel mappings with no canonical reminder.
6. Surface failures without mutating Markdown.

### 9.5 Test seams

Use narrow host interfaces for clock, calendar/time zone, notification authorization, scheduling, and cancellation. Prefer deterministic fake clocks and an in-memory notification scheduler over mocks of individual framework calls.

### 9.6 Tests

- Permission granted, denied, provisional, and revoked.
- Schedule, replace, cancel, and complete actions.
- App restart and reconciliation.
- Index deletion/rebuild and reconciliation.
- Daylight-saving gap and overlap fixtures.
- Device time-zone change.
- Trashed/deleted note cancellation.
- Notification action opens the exact note/task or reports that it no longer exists.
- Duplicate task ID introduced or removed during reconciliation.
- Reminder creation in provisional folders and the adoption/decline flow.
- Notification action racing editor typing and watcher refresh.

### 9.7 Exit gate

- Reminder intent survives index deletion because it remains in Markdown.
- Host scheduling failures never delete or alter canonical annotations.
- Reconciliation is idempotent.
- No notification remains attached to a different task after source edits.

## 10. Phase 6: Hardening

### 10.1 Data integrity

- Fuzz task annotation parsing.
- Run task commands against malformed and adversarial Markdown.
- Test duplicate IDs across notes and within one note.
- Test external edits while task actions and reminder reconciliation run.
- Test save success with reindex failure and save failure after provisional UI updates.
- Test task-view, notification, editor, and watcher races through the per-note executor.
- Confirm index/state loss never loses completion, deadline, or reminder source state.

### 10.2 Performance

- Measure extraction for large notes with many tasks.
- Ensure typing is not blocked by full-library task queries.
- Bound reminder reconciliation and perform platform work asynchronously.
- Verify task view remains responsive on representative large datasets.

### 10.3 Accessibility

- VoiceOver announces checkbox state and task metadata.
- Every task action is keyboard accessible.
- Deadline and reminder controls expose validation errors.
- Notification actions have clear accessible labels.

### 10.4 Diagnostics

- Malformed annotation diagnostics identify note and safe source location without logging content.
- Notification errors distinguish permission, invalid date, platform failure, and missing task.
- Duplicate-ID repair is explicit and reversible before save.

## 11. Milestone Acceptance Checklist

- [ ] Standard Markdown checkboxes appear in the task view.
- [ ] Tasks group predictably by note and heading.
- [ ] Indexing plain checkboxes does not rewrite notes.
- [ ] Completing a task in any view updates the canonical checkbox.
- [ ] Open-note actions use document revisions.
- [ ] Closed-note actions use indexed content hashes and optimistic saves.
- [ ] Open, closed, task-view, and notification mutations share one per-note executor.
- [ ] Deadlines and reminders survive index deletion and rebuild.
- [ ] Reminder notifications open the correct note and task.
- [ ] Reminder intents emit only after canonical save and committed task indexing.
- [ ] Reminder creation requires adopted folder and note identity.
- [ ] Completing or deleting a task cancels its scheduled notification.
- [ ] Permission denial leaves canonical reminder annotations unchanged.
- [ ] Editing around a task cannot silently transfer its reminder.
- [ ] Duplicate IDs are diagnosed and never resolved silently.
- [ ] Duplicate task IDs suppress reminder scheduling until repaired.
- [ ] Invalid annotations remain intact and produce actionable diagnostics.
- [ ] Task operations participate in Rust undo/redo.
- [ ] Keyboard and VoiceOver workflows pass.

## 12. Dependency Order

```text
Task syntax ADR
  -> extraction and identity
  -> editor task commands
  -> task index and rebuild
  -> task view integration
  -> deadlines and reminder host protocol
  -> hardening and acceptance
```

The task view can be prototyped against fixture data after domain types stabilize, but it must not ship before real editor commands and concurrency paths are integrated. Notification UI can be designed in parallel, but scheduling must wait for stable task identity and canonical reminder parsing.

## 13. Completion Artifacts

- Task syntax ADR and fixture corpus.
- Task extraction and editor command APIs.
- Versioned application-ABI task, query, event, and notification messages.
- Task SQLite migration and rebuild support.
- Open-note and closed-note task mutation paths.
- Native macOS task view.
- Notification host adapter and reconciliation service.
- Deterministic clock, calendar, and notification test fakes.
- Milestone acceptance and data-rebuild report.
