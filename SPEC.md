# Howler Product and Architecture Specification

Status: Draft 2  
Audience: Product and engineering  
Working name: Howler

## 1. Summary

Howler is a local-first Markdown notes system built around a reusable core engine. The first product is a native macOS app for quickly capturing and retrieving notes without leaving the current workflow. Future hosts may include mobile apps, a CLI, and integrations with other editors.

The product has two equally important outcomes:

1. A small, reliable engine for storing, parsing, searching, and enriching Markdown notes.
2. A keyboard-first macOS app with an unobtrusive floating editor similar in spirit to Raycast Notes.

Howler stores notes as ordinary Markdown files. Device-local SQLite databases provide disposable indexes and operational state, but neither is the source of truth for note content or task completion. The core is written in Rust, includes a headless editor engine, and is exposed to native hosts through a versioned C ABI.

### 1.1 Terminology

- **Note folder:** The user-selected directory containing Markdown notes, attachments, and minimal portable Howler metadata.
- **Editor library:** The reusable, headless Rust component that owns document editing behavior and has no filesystem or SQLite dependency.
- **Application services:** Rust components that use the editor library and implement Howler-specific note-folder, persistence, indexing, search, task, and recovery workflows.
- **Application state directory:** Device-local SQLite databases, recovery journals, and host state stored outside the note folder.

## 2. Product Principles

### 2.1 Local first

Creating, editing, searching, and organizing notes must work without a network connection or account. The UI must distinguish an accepted edit, a durable recovery draft, and a canonical file save; it must not imply that an in-memory edit has already reached disk.

### 2.2 User-owned data

Notes are readable Markdown files in a user-accessible note folder. A user can back up, inspect, edit, or move the folder without Howler. Rebuilding the database from the files must not lose note content or task completion state.

### 2.3 Capture without interruption

The macOS app must appear quickly, accept input immediately, and avoid unnecessary interface chrome. Common actions must be possible without a mouse.

### 2.4 Reusable Rust core, native hosts

The authoritative text buffer, edit transactions, and Markdown editing semantics belong in the reusable editor library. Storage, search, tasks, and plugin contracts belong in Rust application services layered above it. Glyph layout, native text input, window management, notifications, and platform integrations belong in native host applications.

The shared Rust components must not force every platform into a least-common-denominator user experience.

### 2.5 Preserve source

The editor may render Markdown as rich text, but it must preserve valid source that it does not understand. Howler must not silently rewrite unrelated formatting or remove extension syntax.

### 2.6 Progressive enrichment

A note starts as text. Tasks, dates, repository references, meeting links, and other context are extracted from that text. Users should not need to complete a form before writing.

### 2.7 Safe evolution

File metadata, database schemas, the FFI, and plugin APIs are independently versioned. Unknown fields are preserved where possible, and destructive migrations require a recoverable backup.

## 3. Goals and Non-goals

### 3.1 Goals

- Write and edit Markdown notes.
- Save every note locally and remain fully usable offline.
- Search and switch notes from a command palette.
- Create notes with `Cmd+N` and open the palette with `Cmd+P`.
- Extract Markdown tasks into a unified task view.
- Group tasks and support deadlines and local reminders.
- Provide a reusable core for desktop, mobile, CLI, and editor hosts.
- Coexist safely with external editors and folder synchronization tools.
- Establish a permissioned plugin model for future automations.
- Support contextual references such as GitHub resources, local repositories, dates, locations, and meeting links in later milestones.

### 3.2 Non-goals for the first release

- Real-time multi-user collaboration.
- A hosted web editor.
- Rich document formats that cannot round-trip to Markdown.
- General project management or issue tracking.
- Arbitrary third-party code execution.
- Any built-in device synchronization, sync protocol, or hosted sync service under this specification.
- Mobile applications.
- Compatibility with every Markdown extension.
- WYSIWYG layout such as pages, columns, or free-form canvases.

## 4. Users and Core Workflows

The initial user is an individual who frequently takes transient notes, meeting notes, and task lists while working on a Mac.

### 4.1 Quick capture

1. The user invokes Howler through a configurable global shortcut.
2. A floating editor appears above the current app and restores the last active note.
3. The user types immediately.
4. Changes save automatically.
5. The window hides when dismissed without closing the application.

### 4.2 Create a note

1. The user presses `Cmd+N` while Howler is active.
2. Howler creates an untitled note and focuses the editor.
3. The first suitable line becomes the display title unless the user sets an explicit title.

### 4.3 Find a note

1. The user presses `Cmd+P`.
2. A command palette overlays the editor.
3. Results update as the user types, matching title first and body second.
4. Selecting a result opens it in the same window and restores the prior cursor position where possible.

### 4.4 Manage tasks

1. The user writes standard Markdown checkboxes.
2. Application services index each checkbox as a task and derive its group from the note and heading hierarchy.
3. Optional Howler annotations add deadlines and reminders.
4. Completing a task in either the note or task view updates the checkbox in the Markdown file.
5. The macOS host schedules or cancels local notifications based on application-service reminder events.

## 5. Scope and Delivery Stages

### 5.1 Milestone 1: Local notes and search

- Markdown note-folder creation and selection.
- Create, edit, rename, delete, and restore notes.
- Source-preserving rich Markdown editor.
- Atomic autosave and crash recovery.
- Floating macOS window and global activation shortcut.
- `Cmd+N` note creation.
- `Cmd+P` note search and switching.
- Title and body full-text search.
- Basic settings for note-folder location, shortcuts, and window behavior.
- Core CLI used for development, diagnostics, and index rebuilding.

### 5.2 Milestone 2: Structured tasks

- Checkbox extraction and stable task identity.
- Task grouping by note and heading.
- Deadline and reminder annotations.
- Task list and filtering.
- Local macOS notifications.
- Bidirectional task updates between task views and Markdown.

### 5.3 Milestone 3: Context

- Typed contextual reference model.
- Date, URL, meeting link, GitHub, local repository, and location detectors.
- Context actions supplied by the host or a provider.
- User controls for enabling detectors and resolving ambiguous matches.

### 5.4 Milestone 4: Plugins and additional hosts

- Versioned plugin SDK and capability model.
- Sandboxed plugin runtime.
- Calendar plugin that can create a note for a meeting.
- Additional hosts prioritized from actual usage, such as CLI workflows or mobile.

## 6. System Architecture

### 6.1 Components

```text
+------------------+     +----------------------+     +----------------------+
| Native SDK host  |<--->| Rust app services    |<--->| User-owned note      |
| Zig presentation | FFI | storage/search/tasks |     | folder + attachments |
+--------+---------+     +-----+------------+---+     +----------------------+
         |                     |            |
         v                     v            v
+----------------------+  +----------------------+  +----------------------+
| Rust editor library  |  | Application state    |  | Platform service     |
| no filesystem/SQLite |  | SQLite + recovery    |  | brokers              |
+----------------------+  +----------------------+  +----------------------+
```

### 6.2 Repository shape

The initial repository should be a monorepo:

```text
howler/
  core/                 Rust workspace
    crates/text/        Buffer, ranges, edits, selections, and history; no I/O
    crates/domain/      Domain types and rules
    crates/markdown/    Parsing, source edits, and extraction
    crates/editor/      Reusable headless editor sessions and commands; no I/O
    crates/storage/     Note-folder persistence and application state
    crates/search/      Search API and ranking
    crates/howler/      Howler application-services facade
  ffi/
    editor/             Standalone editor C ABI
    application/        Full Howler application C ABI
  apps/
    native/             Native SDK shell and Zig presentation layer
    cli/                Development and diagnostics CLI
  plugins/
    sdk/                Future plugin schemas and host interfaces
  schemas/              Versioned external data schemas
  docs/
    adr/                 Architecture decision records
```

Crates should only be split when their dependency or testing boundaries justify it. The structure above describes ownership, not a requirement to create every crate on day one.

### 6.3 Editor library responsibilities

The reusable Rust editor library owns:

- Authoritative text buffers and document revisions.
- Edit transactions, logical selections, and selection transforms.
- Editor commands and undo/redo history.
- Markdown parsing and source-level transformations.
- Semantic decorations and source-to-presentation ranges.
- Pure extraction of metadata and context from source.

It has no filesystem, SQLite, recovery-journal, notification, application-lifecycle, plugin-runtime, or synchronization dependency. A host may embed only this layer.

### 6.4 Application service responsibilities

Rust application services own:

- Note-folder discovery and validation.
- Note identity and lifecycle rules.
- Atomic file persistence and optimistic concurrency checks.
- Recovery journals in the application state directory.
- Coordinating extracted metadata and context with indexes and application events.
- Task identity, grouping, deadlines, and reminder state.
- SQLite migrations and index rebuilding.
- Search indexing, queries, and result ranking.
- Domain events consumed by hosts.
- Plugin manifests, capabilities, and event contracts.

### 6.5 Host responsibilities

The macOS host owns:

- Application and window lifecycle.
- Glyph layout, rich text presentation, caret hit testing, input methods, and accessibility.
- Translating committed native text input into editor-library transactions.
- Keyboard shortcuts and command palette presentation.
- File and notification permission prompts.
- Platform file coordination for provider-managed locations.
- Local notification scheduling and delivery.
- Menu bar, Settings, App Intents, and system integration.
- Platform-specific context actions.
- Translating editor and application-service events into UI updates on the appropriate thread.

### 6.6 Boundary rule

The standalone editor API operates only on source, editor transactions, and commands, then returns snapshots, logical selections, decorations, changed ranges, and semantic events. The application API composes editor handles with note-folder and index operations. Neither API exposes SQLite rows, parser implementation objects, Rust pointers with host-managed lifetimes, glyph layout, or platform UI state.

## 7. Technology Choices

### 7.1 Core: Rust

Rust provides memory safety, portable native binaries, mature text-buffer, Markdown, and SQLite libraries, and a practical C ABI path. It supports future CLI, mobile, and WebAssembly targets without coupling editor behavior to Apple frameworks.

The editor library uses an efficient UTF-8 text structure such as a rope. The exact implementation is selected through profiling and editing fixtures rather than exposed in the public API.

The Rust workspace should prefer stable Rust and minimize unsafe code. Unsafe code is isolated to reviewed FFI modules.

### 7.2 Native host: Native SDK and Zig

Native SDK provides rendering, editable Markdown, window behavior, keyboard handling, input methods, and accessibility. Zig adapts Native SDK events to the Rust application session rather than maintaining an independent document model. The first target is macOS 13; Windows and Linux follow macOS parity.

A browser-based editor is not planned because it weakens native behavior and introduces a second application runtime.

### 7.3 Local database: SQLite

SQLite stores derived indexes and device-local operational state outside the note folder. Full-text search should use SQLite FTS5 unless measurement demonstrates that it cannot satisfy the search requirements.

### 7.4 Interoperability: C ABI

The editor library and full application services expose separate opaque-handle C APIs with explicit ownership and error handling. A host that only needs editing does not link storage or SQLite. Zig imports the checked-in C headers, which remain the compatibility contracts.

The FFI is versioned separately from Rust crates. Asynchronous operations use callbacks or a polled event queue; they must never invoke host UI code while holding internal Rust locks.

### 7.5 Cross-platform targets

Rust hosts and the CLI import the editor crate directly. Swift, Kotlin/JNI, C, C++, Python, and native JavaScript addons consume the C ABI through runtime-specific wrappers. Browser use compiles the editor library through a dedicated WebAssembly adapter rather than exposing application services that assume native paths and SQLite.

Cross-platform support means shared document behavior, not shared rendering. Every graphical host implements a native adapter for input, layout, selection display, accessibility, and decorations. Shared transaction fixtures must produce identical editor snapshots and history regardless of host.

## 8. Note Folder

### 8.1 Canonical layout

```text
Howler Notes/
  project-kickoff.md
  projects/
    roadmap.md
  attachments/
    diagram.png
  .trash/
  .howler/
    library.json        Stable library identity and format version
```

Howler can open an existing folder and discovers Markdown files recursively, excluding `.howler`, `.trash`, and user-configured ignored paths. A note folder created by Howler uses the layout above, but existing folders do not need to adopt the suggested directory names.

File names are human-readable but are not identity. Moving or renaming an adopted note file does not change its note ID. Files without `howler_id` have only provisional identity as described in Section 8.5.

Device-local state is stored outside the note folder:

```text
Application Support/Howler/
  folders/<folder-state-id>/
    index.sqlite3       Disposable derived index
    state.sqlite3       Device-local operational state
    recovery/           Unsaved-edit journals
```

This state is never expected to be synchronized with the note folder. User-authored note and task content must not depend on it for interpretation. Losing it may lose recovery drafts and local UI state, but must not alter active Markdown notes. The `.howler/library.json` file contains no device-specific paths, credentials, cursors, or UI state.

For an adopted folder, `folder-state-id` is its stable ID from `.howler/library.json`. Before adoption, it is a device-local key derived from the canonical root path and filesystem volume identity, supplemented by a platform bookmark or file identifier where available. Moving an unadopted folder may require the user to select it again and may lose its local UI/index association, but not note content. When the folder is adopted, application services migrate provisional state to the stable folder ID atomically.

### 8.2 Note format

Every discovered note is UTF-8 Markdown. An adopted note has minimal YAML front matter:

```markdown
---
howler_id: 01J6Y3Q9E3D2K7M8F4A1B6C5T0
howler_created_at: 2026-08-22T09:30:00Z
---

# Project kickoff

- [ ] Send agenda @due(2026-08-24) @remind(2026-08-24T09:00:00-07:00)
  <!-- howler-task-id: 01J6Y4B4N8F9A2C7D1E5G3H6K0 -->
```

The exact task annotation grammar must be validated in an ADR before Milestone 2. Its required properties are:

- Valid and readable as ordinary Markdown.
- Predictable to parse without natural-language guessing.
- Stable across formatting changes.
- Preserved by other Markdown editors.
- Capable of representing timezone-aware reminders.

The annotation ADR must also define daylight-saving behavior and whether named time zones are preserved in addition to an absolute offset.

Howler reserves only front-matter keys prefixed with `howler_` and HTML metadata comments prefixed with `howler-`. It recognizes `title` as user-authored display metadata but does not reserve it for machine state. Unrecognized front matter and Markdown are preserved.

### 8.3 Titles

Title precedence is:

1. Explicit `title` front-matter value, if present.
2. First level-one heading.
3. First non-empty plain-text line.
4. `Untitled`.

The title is indexed metadata. The user-facing Rename command changes the explicit title or heading according to an editor preference; it does not rename the file. File renaming is a separate note-folder operation.

### 8.4 Writes and recovery

Application services associate each note-backed editor handle with the disk content hash from which it began. The standalone editor library has no knowledge of this hash. Saving a note follows this sequence:

1. Persist the current source and base hash to the recovery journal.
2. Read and hash the current destination immediately before writing.
3. If that hash differs from the session's base disk hash, stop and return an external-conflict result without changing the destination or recovery journal.
4. Serialize the complete UTF-8 source to a temporary file in the destination directory.
5. Flush and synchronize the temporary file using the strongest supported platform primitive.
6. Acquire platform file coordination or an advisory application lock where available.
7. Re-read and validate the destination hash while coordinated; on mismatch, return an external conflict and retain recovery data.
8. Atomically replace the destination and synchronize the parent directory where the platform permits it.
9. Update the application's base disk hash, consider the canonical note saved, and remove its recovery journal.
10. Record the resulting content hash and file metadata in SQLite.
11. Parse and update derived indexes in a transaction.
12. Emit note and task events after the transaction commits.

Atomic replacement prevents torn files but is not a universal compare-and-swap operation. An uncoordinated external writer can still race between validation and replacement on some filesystems. Howler uses host-provided file coordination on macOS file-provider locations, suppresses and reconciles its own watcher events, retains recovery data until replacement succeeds, and documents the residual race rather than claiming absolute concurrent-write exclusion.

If the canonical write succeeds but indexing fails, the save still succeeds. Application services mark the note's index stale, emit a diagnostic, and retry asynchronously. Search may temporarily omit the latest content, but opening the note always reads the canonical file. A later rebuild repairs stale derived state.

Application services may debounce disk writes briefly, but they must maintain a recovery journal before reporting an edit as `recovery_durable`. On clean save, the journal entry is removed. Startup offers recovery if journal content is newer than the canonical file.

### 8.5 External changes

The storage layer watches the note folder for external file operations. It must handle:

- A known note modified outside Howler.
- A new Markdown file without Howler metadata.
- File rename or move.
- Duplicate note IDs caused by copying.
- Deletion outside Howler.
- Temporary files and partial writes from other tools.

If the open note changes externally while it has unsaved in-memory edits, Howler preserves the known in-memory and external versions and requests a user decision. It never overwrites a detected divergent version based only on modification time.

Selecting an existing folder includes an explicit adoption choice. If accepted, Howler creates `.howler/library.json` and assigns missing note IDs with atomic metadata-only writes before treating those files as managed notes. A user may refuse source mutation and open the folder with provisional identities; move-stable identity, reminders, and other identity-dependent features are then unavailable until adoption.

A file discovered later receives a provisional identity derived from its normalized relative path and content hash. Indexing alone does not rewrite it. Opening, editing, or explicitly adopting it may add `howler_id` after confirmation. Unadopted files remain valid notes without front matter. If an ID is removed or changed externally, Howler treats that as an identity change and offers repair rather than restoring metadata silently. Copied or conflict files with duplicate IDs remain separately addressable by provisional path identity until the user repairs them.

Path handling must define case folding and Unicode normalization per filesystem, reject paths that escape the note folder, and avoid following symlinked directories outside the folder by default. `.howler/library.json` is optional until a folder is adopted.

### 8.6 Deletion

Deleting a note moves it into the note folder's `.trash` directory. An attachment is moved only after a fresh reference scan proves that no active note links to it. Trash retention defaults to 60 days and is configurable. Restore preserves the note ID. Permanent deletion is explicit.

## 9. SQLite Data Model

SQLite is an implementation detail, not a public API. Howler separates a disposable derived index from device-local operational state. Both live in platform application support rather than the user-selected note folder.

The initial logical tables are:

| Table | Store | Purpose |
| --- | --- | --- |
| `notes` | Index | File mapping, title, timestamps, hashes, and indexing status |
| `note_fts` | Index | FTS5 title and body index |
| `tasks` | Index | Derived task location, text, state, group, deadline, and reminder |
| `contexts` | Index | Derived typed references and source ranges |
| `attachments` | Index | Derived reference, hash, media type, and path |
| `plugin_metadata` | State | Namespaced, size-limited plugin state |
| `host_state` | State | Local-only UI state such as cursor and recent notes |
| `migrations` | Both | Applied schema versions for each database |

All source ranges are stored against a content hash. A range from an older hash cannot be applied to new content without reparsing.

The index database can be reconstructed from Markdown and attachments. An index rebuild leaves the state database and recovery directory intact. Device backups may include local state, but copying the note folder alone remains sufficient to preserve canonical notes and tasks.

## 10. Domain Model

### 10.1 Note

```text
Note
  id: NoteId
  relative_path: Path
  title: String
  source: UTF-8 Markdown
  created_at: Instant
  modified_at: Instant
  content_hash: Hash
  lifecycle: active | trashed
```

`modified_at` is useful for display but is not a document revision or conflict-resolution clock.

### 10.2 Task

```text
Task
  id: TaskId
  note_id: NoteId
  source_range: Range
  source_content_hash: Hash
  text: String
  completed: Boolean
  group: HeadingPath
  deadline: Optional<LocalDate>
  reminder: Optional<ZonedInstant>
```

The Markdown checkbox and annotations are canonical. The task row is derived. A source-level edit produced by the Markdown module is the only supported way to update a task from outside the editor.

Task IDs are embedded metadata because line numbers and text are not stable enough for reminders, cross-note references, or external edits. Application services allocate an ID when a task first requires stable external identity and invoke an editor command to insert it. Plain checkboxes without IDs are still indexed using a temporary identity derived from their note, source range, and content hash.

### 10.3 Task group

A task group is the containing note plus heading path. Explicit cross-note groups may be added later but are not part of Milestone 2.

### 10.4 Reminder

A reminder is derived from a task annotation. Application services compute desired reminder state and emit scheduling or cancellation events. The host owns the platform notification identifier and reports scheduling results to application services.

### 10.5 Context reference

```text
ContextReference
  id: derived identifier
  note_id: NoteId
  kind: date | url | meeting | github | repository | location | custom
  source_range: Range
  display_text: String
  normalized_target: String
  provider: String
  confidence: Optional<Number>
```

Context references are derived and may overlap. Detection must not mutate Markdown automatically.

### 10.6 Attachment

Markdown relative links are canonical attachment references. The index derives an attachment's normalized path, content hash, media type, and referencing notes from files and links. The content hash is its portable identity; there is no separate attachment identity held only in SQLite. Shared files are not deleted until a fresh reference scan confirms that no active note links to them.

## 11. Editor Core and Markdown Engine

Each open note has a headless Rust `EditorSession`. The session owns:

- The authoritative UTF-8 source buffer.
- A monotonic in-process document revision.
- Logical selections and their transformation through edits.
- Transaction grouping and undo/redo history.
- Markdown syntax state and source ranges.
- Semantic decorations for native presentation.
- Editor commands such as emphasis, links, lists, and task toggles.
- Changed ranges and domain events consumed by storage and hosts.

The buffer should use an efficient text structure such as a rope and must not require copying the complete document for each keystroke. Internal ranges are UTF-8 byte offsets at valid code-point boundaries. Host adapters are responsible for explicit conversion to platform range units such as macOS UTF-16 text ranges.

The Markdown module provides:

- Parsing into a syntax tree with source offsets.
- Incremental reparsing or bounded reparse regions where practical.
- Extraction of headings, links, checkboxes, annotations, and context candidates.
- Source-preserving edits for known operations.
- Plain-text and semantic spans that hosts may use for accessibility.
- Semantic decorations consumed by native editor adapters.

The complete Markdown source in `EditorSession` is always authoritative. Rich presentation is a non-destructive projection using decorations and source ranges; Howler never reconstructs the document by serializing rich text. Editor and command operations mutate the smallest necessary source ranges and leave unrelated bytes unchanged.

### 11.1 Edit transaction contract

A host submits one or more replacements as an edit transaction containing:

- The expected document revision.
- Replacement ranges and UTF-8 text.
- Logical selections before or after the native operation.
- A history-grouping hint, such as typing, paste, formatting, or isolated command.

The editor library validates ranges, applies replacements in a defined order, transforms selections, updates history, reparses affected syntax, and returns the new revision, changed ranges, logical selections, decorations, and events. It rejects a transaction whose expected revision is stale; the host must refresh its snapshot rather than applying ranges to a different document.

Formatting and task operations are editor-library commands, not host-authored string rewrites. This keeps behavior consistent across macOS, mobile, CLI, and future editor integrations.

### 11.2 Native input boundary

The host owns key interpretation, caret hit testing, marked text, dictation, autocorrection, and platform accessibility. Rust supplies semantic spans and text projections but never constructs an accessibility tree, handles accessibility focus, or performs platform actions and announcements.

The native adapter follows this composition protocol:

1. A native text mirror and every logical selection are tagged with the Rust document revision they represent.
2. IME marked text is tracked as a host-local overlay or reversible temporary mutation over a known source range; Rust remains at the pre-composition revision.
3. Before an editor-library command changes that document, the host commits or explicitly cancels active composition.
4. Composition commit produces one revision-checked transaction containing the original source range and committed UTF-8 text.
5. On a stale-revision rejection, the host retains the composed payload, refreshes the Rust snapshot, and replays only if the original range and base text still match; otherwise it surfaces a conflict without dropping input.
6. External file replacement during composition commits or preserves the composition draft before the session processes the external version.

Selections carry anchor and head offsets, direction, affinity, and their document revision. Offsets must lie on UTF-8 code-point boundaries; platform grapheme navigation is converted explicitly. The model permits multiple selections even if an initial host exposes only one.

Undo and redo history is authoritative in the Rust session. The host maps native undo commands and menu state to core operations. Host grouping hints are advisory; deterministic Rust rules decide final history boundaries so identical transaction streams produce identical history across platforms.

### 11.3 Note-backed editor protocol

The full Howler application does not expose a raw `EditorHandle` for an open note. Application services wrap it in a `NoteEditorHandle` containing the editor session, note identity, base disk hash, recovery generation, and save state.

Every note-backed edit, command, undo, or redo enters through application services. They invoke the editor library serially and, only after a transaction is accepted, schedule the resulting source and revision for recovery persistence and canonical autosave. This guarantees that application services observe every accepted note mutation. Standalone editor consumers use `EditorHandle` directly and receive no persistence guarantees.

### 11.4 Markdown compatibility

The baseline dialect is CommonMark plus GitHub-style task list items, tables, strikethrough, and autolinks. Front matter and Howler annotations are extensions. The precise parser library and compatibility fixtures are implementation decisions.

### 11.5 Unknown syntax

Unknown HTML, front matter, extension markers, and formatting must survive loading and saving. Operations should replace the smallest necessary source range instead of serializing the entire parsed document.

## 12. Editor Experience

### 12.1 Presentation

The default is one source-preserving rich Markdown editor:

- Headings, emphasis, links, code, lists, and checkboxes render inline.
- Syntax punctuation may be subdued or hidden outside the active construct.
- Moving the caret into a construct reveals enough syntax to edit it directly.
- Pasting Markdown inserts Markdown semantics rather than flattened rich text.
- Copy supports plain text and Markdown; rich HTML is optional.
- A plain-source mode is a useful fallback but not required for Milestone 1.

The Native SDK editor adapter applies editor-library decorations to the platform text system without becoming a second source of document semantics. The native backend owns glyph layout, display attributes, the accessibility tree, accessibility ranges and actions, focus, and announcements. Rust owns which source ranges represent headings, emphasis, links, tasks, syntax markers, and other semantic constructs.

### 12.2 Autosave

Typing submits transactions through the note-backed application-service handle immediately after native input handling. Application services expose three distinct states: `accepted` in memory, `recovery_durable`, and `file_saved`. The standalone editor library exposes only accepted document revisions. Recovery persistence occurs within 250 ms of inactivity, and canonical file persistence occurs within 750 ms of inactivity. Losing focus, switching notes, hiding the window, or terminating the app forces an immediate save attempt.

Save errors remain visible and retryable. The app must not close a dirty note without either persisting recovery data or receiving explicit confirmation.

### 12.3 Undo and redo

Typing, formatting commands, checkbox changes, and task-view actions participate in the editor session's coherent undo history while the note is open. An external modification starts a new history boundary.

### 12.4 Accessibility and input

The editor must support VoiceOver, keyboard navigation, dictation, text substitutions, emoji input, marked text, and common input methods. These are acceptance requirements, not post-release polish.

## 13. macOS Application

### 13.1 Window behavior

- The primary editor is a floating panel without permanent custom chrome.
- The panel can be pinned above other application windows.
- A configurable global shortcut shows or hides the panel.
- Howler remembers size and position per display arrangement.
- The window remains usable across Spaces and full-screen contexts subject to macOS restrictions.
- The window may grow with content within user- and screen-bounded limits; beyond them, content scrolls.
- Search, task views, and transient controls overlay or temporarily replace editor content rather than creating a permanent sidebar.

### 13.2 Commands

| Command | Default | Behavior |
| --- | --- | --- |
| Show or hide Howler | Configurable | Global activation |
| New note | `Cmd+N` | Create and focus a note |
| Open palette | `Cmd+P` | Search and switch notes |
| Close or hide | `Esc` | Dismiss transient UI, then hide panel |
| Toggle pin | Configurable | Change floating level |
| Show tasks | Configurable | Open task view |

Shortcuts must be configurable where global or likely to conflict. Standard text editing shortcuts remain native.

### 13.3 Application lifecycle

Closing the panel keeps the lightweight application available for global activation. The user can quit explicitly. Startup restores the last active note, cursor position, window geometry, and pin state after validating that the referenced note still exists.

### 13.4 Notifications

The app requests notification permission only when a user creates the first reminder or enables reminders in settings. Notification actions may complete the task or open its note. The host reports authorization and scheduling failures without changing canonical reminder annotations.

## 14. Search and Command Palette

### 14.1 Indexing

Search indexes normalized title and plain-text body. Markdown syntax, Howler metadata comments, and front matter reserved for machine state are excluded from body terms. Index updates follow successful file writes and are transactional.

### 14.2 Ranking

Initial ranking considers:

1. Exact and prefix title matches.
2. Fuzzy title matches.
3. Body term matches.
4. Recency as a bounded tie-breaker.

Ranking must be deterministic for the same query and index state. Later usage signals must not make old notes impossible to find.

### 14.3 Palette behavior

- Opens with recent notes when the query is empty.
- Updates results without blocking editor input.
- Supports keyboard selection and dismissal.
- Highlights why a result matched.
- Treats opening a note as the primary action.
- May add commands later, but Milestone 1 is primarily note retrieval.

## 15. Data Flows

### 15.1 Create and edit

```text
Host -> App services: create_note
App services -> Files: atomically create Markdown file
App services -> SQLite: index note
App services -> Editor library: create editor session
App services -> Host: note-editor handle and initial snapshot
Host: present source and decorations through the Native SDK text control
Host -> App services: apply note edit(expected_revision, edits, selections)
App services -> Editor library: apply transaction
Editor library -> App services: accepted revision, selections, changes, decorations
App services -> Host: accepted edit result
App services -> Recovery: schedule recoverable draft
App services -> Files: validate base hash and atomically replace note
App services -> SQLite: update note, FTS, tasks, and contexts
App services -> Host: saved snapshot and domain events
```

### 15.2 Search and open

```text
Host -> App services: search(query, limit)
App services -> SQLite: FTS and metadata query
App services -> Host: ranked summaries
Host -> App services: open_note(note_id)
App services -> Files: read and verify content
App services -> Editor library: create session from source
App services -> Host: note-editor handle, source, decorations, and document revision
```

### 15.3 Complete a task outside the editor

For a note already open, the task action carries its `NoteEditorHandle` and expected document revision:

```text
Host -> App services: set_open_task_completed(note_editor, task_id, true, expected_revision)
App services -> Editor library: execute task-toggle command and update history
App services -> Files: validate base hash and atomically save edited note
App services -> SQLite: reparse and update derived task
App services -> Host: note_changed and reminder_cancel events
```

For a closed note, the indexed task carries the note content hash against which its source range was derived. Application services re-read the file, reject and reindex if the hash changed, create a transient editor session, execute the same editor command, and perform the optimistic-concurrency save against that hash. A task ID needed for stable external state is allocated by application services and supplied to the editor command that inserts its metadata.

### 15.4 External modification

```text
File watcher -> App services: path changed
App services -> Files: wait for stable readable content
App services: compare identity and content hash
App services -> Editor library: refresh clean session or preserve dirty session as a conflict
App services -> SQLite: reindex accepted file content
App services -> Host: refreshed snapshot or conflict event
Host: update presentation or request conflict resolution
```

### 15.5 Reminder scheduling

```text
App services -> Host: desired_reminder(task_id, instant, content)
Host -> macOS: schedule local notification
Host -> App services: reminder_schedule_result
```

## 16. File Coexistence and Future Sync

Howler has no built-in synchronization under this specification. The note folder is an ordinary directory that a user may place under Dropbox, Syncthing, Git, a platform file provider, or a future dedicated sync tool. Those systems remain outside the application architecture.

### 16.1 Coexistence requirements

- Canonical notes and task state live in Markdown files.
- Attachments use relative links and remain inside the selected note folder unless explicitly linked externally.
- SQLite, recovery journals, cursor state, and window state remain outside the note folder.
- Writes use same-directory temporary files and atomic replacement where the filesystem supports it.
- Application services watch for create, modify, move, and delete events and verify content hashes before acting.
- Howler tolerates delayed, duplicated, reordered, and coalesced filesystem notifications by rescanning affected paths.
- A newly discovered conflict copy is indexed as a separate note rather than discarded.
- Howler does not hold long-lived exclusive locks that prevent other editors or synchronization tools from operating.

### 16.2 External conflicts

Generic folder synchronization cannot provide reliable application-level revision history or automatic conflict resolution. When an open note changes externally:

- A clean editor session refreshes to the external version and starts a new history boundary.
- A dirty editor session preserves both the in-memory and external versions.
- Howler may offer a three-way merge only when it has a known local base snapshot.
- An ambiguous merge requires explicit user resolution or creates a separate conflict note.
- Howler never silently overwrites either complete version based only on modification time.

Duplicate `howler_id` values, including those produced by conflict copies, are detected and surfaced. Repair assigns a new ID to the chosen copy without rewriting unrelated content.

### 16.3 Separate future solution

A future synchronization product requires a separate architecture, protocol specification, and threat model; no sync protocol, service API, or editor extension point is defined here. Such a product must consume the same file format and must not make the editor dependent on an account or network.

## 17. Context System

Context detection is an enrichment pipeline over parsed note content. Each provider declares supported reference kinds and receives eligible source spans. It returns normalized references and available actions.

Examples include:

- GitHub issue or pull request URLs.
- Local repository paths and commit hashes.
- ISO dates and explicit Howler date annotations.
- Geographic links or structured location annotations.
- Zoom, Meet, and Teams links.

Context providers must not perform network requests during typing. Network enrichment is asynchronous, cached, cancellable, and visually distinct from locally derived information.

Ambiguous natural-language dates or locations must not create reminders or change source without confirmation.

## 18. Plugin Architecture

Plugins are deferred until the application-service event and command boundaries have been exercised by native code.

### 18.1 Model

A plugin package contains a versioned manifest, executable module, and optional resources. WebAssembly is the preferred future runtime because it offers portability and controllable capabilities. The final runtime requires a separate prototype and security review.

### 18.2 Capabilities

Plugins request narrowly scoped capabilities such as:

- Read selected note.
- Read notes matching a user-approved query.
- Create a note.
- Propose an edit to a note.
- Register commands.
- Subscribe to selected domain events.
- Register context detectors and actions.
- Make network requests to approved origins.
- Read calendar events through a host broker.
- Store namespaced metadata within a quota.

There is no generic filesystem, process, environment, credential-store, SQLite, or unrestricted network access.

### 18.3 Execution rules

- Capability grants are visible and revocable.
- Event delivery is asynchronous and idempotent where possible.
- Plugins have CPU, memory, storage, and request limits.
- A plugin cannot block saving or editor input.
- Plugin failures are isolated and inspectable.
- Note edits are validated by the editor library and attributed to the plugin.
- Plugins cannot render arbitrary permanent UI in the editor.

### 18.4 Calendar example

The calendar plugin receives user-approved upcoming events through the host calendar broker. According to user rules, it creates a note from a template and records the external event ID in namespaced metadata to prevent duplicate creation.

## 19. Public API Shape

### 19.1 Standalone editor API

The reusable editor contract has no path, filesystem, SQLite, recovery, or note-folder concepts:

```text
create_editor(source) -> EditorHandle + EditorSnapshot
apply_edits(editor, transaction) -> EditResult
execute_editor_command(editor, command) -> EditResult
undo(editor) -> Optional<EditResult>
redo(editor) -> Optional<EditResult>
snapshot(editor) -> EditorSnapshot
decorations(editor, range, expected_revision) -> Decorations
destroy_editor(editor)
```

### 19.2 Application API

The full Howler facade composes editor handles with coarse application operations:

```text
open_note_folder(path) -> NoteFolderHandle
create_note(initial_source?) -> NoteSnapshot
open_note_editor(note_id) -> NoteEditorHandle + EditorSnapshot
apply_note_edits(note_editor, transaction) -> EditResult
execute_note_command(note_editor, command) -> EditResult
undo_note_editor(note_editor) -> Optional<EditResult>
redo_note_editor(note_editor) -> Optional<EditResult>
save_note_editor(note_editor, expected_revision) -> NoteSnapshot
delete_note(note_id) -> DeletedNote
restore_note(note_id) -> NoteSnapshot
search_notes(query, limit) -> SearchResults
list_tasks(filter) -> Tasks
set_open_task_completed(note_editor, task_id, completed, expected_revision) -> EditResult
set_indexed_task_completed(task_id, completed, expected_content_hash) -> NoteSnapshot
poll_events() -> Events
rebuild_index() -> RebuildReport
```

Names illustrate responsibilities and are not a frozen ABI. Every mutating editor operation is explicit about the expected document revision. File persistence and external-change checks additionally use content hashes. Errors use stable machine-readable codes plus diagnostic text.

`EditorHandle` and `NoteEditorHandle` are distinct opaque types across the C ABI and cannot be interchanged. Complex transactions, selections, snapshots, and results use versioned, length-delimited messages or generated bindings rather than exposing Rust layouts. Hosts must explicitly release returned buffers and handles according to the ownership contract. Unknown optional fields are ignored; incompatible major message versions fail explicitly.

An editor handle is confined to one serial executor and rejects concurrent mutation. Immutable returned snapshots may cross threads. Application-service work such as indexing is asynchronous, cancellable, and reports progress through a polled event queue. The ABI documents allocator ownership, handle lifetime, callback thread, cancellation, and reentrancy rules; UI hosts must not infer thread safety.

## 20. Privacy and Security

- Howler does not transmit notes. External folder synchronization and permitted integrations operate under their own user configuration.
- Telemetry is off by default in the initial product.
- Crash reports are stored locally and require explicit user action before transmission.
- Logs exclude note content, search queries, credentials, and full paths by default.
- Secrets use Keychain through the native host, not Markdown or SQLite.
- External links and plugin network destinations are treated as untrusted.
- Markdown rendering does not execute scripts or load remote resources automatically.
- Note-folder file permissions should be as restrictive as platform defaults allow.
- Plugin execution requires a dedicated threat model before implementation.

## 21. Reliability and Performance

Initial targets, measured on a supported Apple Silicon Mac with a warm local filesystem:

| Measure | Target |
| --- | --- |
| Show an already running panel | p95 under 100 ms |
| Editor accepts input after cold launch | p95 under 500 ms |
| Recovery draft persisted after idle | within 250 ms |
| Canonical note saved after idle | within 750 ms |
| Search over 10,000 medium notes | p95 under 100 ms |
| Open a 1 MB note | p95 under 250 ms |
| Index rebuild | Reports progress and remains cancellable |

Correctness takes priority over these targets. Performance work must be driven by representative measurements.

Reliability requirements:

- A process crash during save leaves either the previous complete file or the next complete file on supported local filesystems. Howler documents weaker guarantees for filesystems that do not provide atomic replacement and durability primitives.
- Index corruption can be repaired without changing note files.
- A malformed note remains editable and exportable.
- One malformed note cannot prevent the note folder from opening.
- Background parsing, file watching, plugins, and search cannot block typing.
- Every failed write is surfaced and retained for retry or recovery.

## 22. Testing Strategy

### 22.1 Rust tests

- Unit tests for domain rules and source-level edits.
- Transaction tests for multi-edit ordering, revision checks, selection transforms, and undo grouping.
- Fixture tests for Markdown extraction and preservation.
- Property tests for arbitrary UTF-8 edits and parser ranges.
- Migration tests from every supported schema version.
- Filesystem fault tests for interrupted and denied writes.
- Index rebuild equivalence tests.
- Cross-platform fixtures that require identical editor results for the same source and transaction sequence.
- Composition adapter fixtures for commit, cancellation, stale revisions, and external replacement.
- Concurrent-writer tests covering pre-save hash mismatches and the residual check-to-replace race.
- File-watcher tests for self-generated events, coalesced events, file-provider replacements, and rescans.

### 22.2 FFI tests

- ABI header compilation from C and translation by Zig.
- Ownership, cancellation, threading, and error-path tests.
- Compatibility tests for supported ABI versions.

### 22.3 macOS tests

- Unit tests for host adapters and view models.
- Integration tests for save, search, task, and notification flows.
- Adapter tests for UTF-8/UTF-16 range conversion and editor/native selection agreement.
- UI tests for keyboard-only editor workflows.
- VoiceOver and input-method manual test passes.
- Window behavior tests across displays, Spaces, and full-screen apps.

### 22.4 End-to-end data tests

Golden note folders should cover external edits, duplicate IDs, malformed front matter, Unicode and case-colliding file names, symlinks, large notes, shared attachments, task annotations, trash recovery, and index deletion/rebuild.

## 23. Observability and Diagnostics

Rust application services emit structured diagnostic events with severity, stable code, operation ID, and redacted context. The CLI can:

- Validate a note folder.
- Rebuild its index.
- Report duplicate IDs and malformed metadata.
- Show schema, editor, and application-service versions.
- Export a redacted diagnostic bundle.

Diagnostics must not upload automatically. Debug logging that includes content requires an explicit temporary opt-in and clear warning.

## 24. Distribution and Compatibility

The first app targets currently supported macOS versions, with the exact minimum selected before implementation based on required text and window APIs. Distribution should support signed and notarized direct downloads. Mac App Store distribution is optional and must not compromise user-selected note-folder access or global shortcut behavior.

Compatibility promises are separate:

- Markdown note-folder format: long-lived and migration-safe.
- Editor and application C ABIs: independently versioned with explicit support windows.
- SQLite schema: internal and migratable.
- Plugin API: unstable until a dedicated SDK release.

## 25. Acceptance Criteria

### 25.1 Milestone 1

- A user can create or open a note folder without an account or network.
- Notes are readable and editable as Markdown outside Howler.
- The app recovers an edit after forced termination during typing.
- `Cmd+N` creates and focuses a new note.
- `Cmd+P` finds notes by title and body using only the keyboard.
- The editor round-trips supported and unknown Markdown without unrelated rewrites.
- The panel can be shown globally, hidden, moved, resized, and pinned above other apps.
- Deleting the SQLite index and rebuilding it restores searchable note content.
- Delete and restore preserve note identity.
- Save failures are visible and do not discard the in-memory or recovery copy.
- A detected external write conflict preserves both known versions and does not overwrite the disk file.

### 25.2 Milestone 2

- Standard Markdown checkboxes appear in the task view.
- Tasks group predictably by note and heading.
- Completing a task in any view updates the canonical Markdown checkbox.
- Deadlines and reminders survive index deletion and rebuild.
- Reminder notifications open the correct note and task.
- Editing around a task does not silently transfer its reminder to another task.
- Invalid annotations remain intact and produce actionable diagnostics.

## 26. Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Native rich Markdown editing is complex | Build a source-preservation prototype first; retain a plain-source fallback |
| Rust and native editor state diverge | Make Rust authoritative, use revision-checked transactions, and test range conversions |
| IME conflicts with authoritative editor state | Keep marked text host-local and commit at defined composition boundaries |
| FFI slows iteration | Keep a coarse application facade and test bindings continuously |
| External editors create identity conflicts | Detect duplicates, avoid rewrites during indexing, provide explicit repair |
| Hidden task IDs surprise users | Use valid comments sparingly and document why stable IDs are needed |
| Files and database diverge | Treat files as canonical and make index rebuild a tested product feature |
| Folder synchronization changes files unexpectedly | Watch and hash files, preserve dirty sessions, and never resolve by timestamp alone |
| Plugins expand the security surface | Defer runtime, require capabilities, sandbox execution, and threat-model it |
| Context extraction creates false positives | Keep extraction non-mutating and require confirmation for consequential actions |

## 27. Open Decisions

The following require prototypes or ADRs before their milestone begins:

1. Exact Markdown parser and source-offset strategy.
2. Rope/text-buffer implementation, transaction model, and undo grouping rules.
3. Native rich editor adapter and minimum supported macOS version.
4. Task annotation grammar and insertion policy for stable task IDs.
5. Global shortcut default and accessibility permission behavior.
6. C ABI message encoding and binding-generation strategy.
7. Plugin runtime choice after a WebAssembly capability prototype.
8. Attachment import, deduplication, and garbage-collection policy.
9. Product licensing and distribution channels.

## 28. Implementation Order

1. Create a Rust editor spike with a rope, revision-checked transactions, selection transforms, Markdown decorations, and undo/redo.
2. Validate the editor C ABI with the Native SDK host, including UTF-8/UTF-16 ranges, IME, and VoiceOver behavior.
3. Add folder discovery, atomic Markdown persistence, SQLite FTS5 indexing, and index rebuilding.
4. Implement note lifecycle, recovery journals, external-change handling, and golden note-folder tests.
5. Build the floating macOS shell, global activation, `Cmd+N`, and `Cmd+P`.
6. Stabilize Milestone 1 against its acceptance criteria.
7. Decide task annotation syntax and implement task extraction, source edits, and notifications.
8. Stabilize Milestone 2 before implementing context or third-party plugins.

This order tests the highest-risk assumptions early: the headless editor contract, source preservation, native input integration, safe persistence, and the cross-language boundary.
