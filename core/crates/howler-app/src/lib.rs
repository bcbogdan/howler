#![forbid(unsafe_code)]

use howler_editor::{
    front_matter_end, Decoration, EditResult, EditorCommand, EditorError, EditorSession, Snapshot,
    Transaction,
};
use rusqlite::{params, Connection, Transaction as SqlTransaction};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tempfile::NamedTempFile;
use thiserror::Error;
use ulid::Ulid;
use walkdir::{DirEntry, WalkDir};

pub const APPLICATION_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const INDEX_SCHEMA_VERSION: u32 = 2;
pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("editor error: {0}")]
    Editor(#[from] EditorError),
    #[error("path escapes the note folder")]
    PathEscape,
    #[error("destination already exists: {0}")]
    DestinationExists(PathBuf),
    #[error("note not found: {0}")]
    NoteNotFound(String),
    #[error("duplicate adopted note ID must be repaired before mutation: {0}")]
    DuplicateIdentity(String),
    #[error("note is not valid UTF-8: {0}")]
    InvalidUtf8(String),
    #[error("external change conflicts with the open editor")]
    ExternalConflict { external_source: String },
    #[error("folder metadata is malformed: {0}")]
    MalformedMetadata(String),
    #[error("title must be a single line")]
    InvalidTitle,
    #[error("note editor belongs to another folder context")]
    WrongOwner,
    #[error("recovery draft not found: {0}")]
    RecoveryNotFound(String),
    #[error("recovery must be restored or discarded before opening this note: {0}")]
    RecoveryPending(String),
    #[error("note identity is invalid or changed externally")]
    IdentityChanged,
    #[error("note editor is stale because the note was moved or trashed")]
    StaleHandle,
    #[error("note is already open in another application session: {0}")]
    NoteAlreadyOpen(String),
    #[error("pending native input must be resolved before mutating note: {0}")]
    PendingNativeDraft(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Identity {
    Adopted(String),
    Provisional(String),
}

impl Identity {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Adopted(value) | Self::Provisional(value) => value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: Identity,
    pub relative_path: PathBuf,
    pub title: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    ExactTitle,
    PrefixTitle,
    FuzzyTitle,
    Body,
    Recent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub note: NoteSummary,
    pub snippet: String,
    pub reason: MatchReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: String,
    pub code: String,
    pub relative_path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebuildReport {
    pub indexed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RescanReport {
    pub notes: usize,
    pub rebuild: RebuildReport,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryDraft {
    pub note_id: String,
    pub relative_path: PathBuf,
    pub revision: u64,
    pub base_hash: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingNativeDraftRecord {
    note_id: String,
    relative_path: PathBuf,
    base_revision: u64,
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NoteCreationRecord {
    operation_id: String,
    note: NoteSummary,
    request_hash: String,
    source_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityState {
    Accepted,
    RecoveryDurable,
    FileSaved,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationOutcome {
    pub edit: Option<EditResult>,
    pub durability: DurabilityState,
    pub recovery_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    Removed,
    AlreadyAbsent,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexState {
    Current,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveOutcome {
    pub revision: u64,
    pub durability: DurabilityState,
    pub recovery_cleanup: CleanupState,
    pub recovery_cleanup_error: Option<String>,
    pub index_state: IndexState,
    pub index_error: Option<String>,
    pub recency_error: Option<String>,
    pub canonical_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconcileResult {
    Unchanged,
    Refreshed { revision: u64 },
    Conflict { external_source: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticBundle {
    pub application_version: String,
    pub editor_version: String,
    pub index_schema: u32,
    pub state_schema: u32,
    pub adopted: bool,
    pub note_count: usize,
    pub recovery_count: usize,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Default)]
struct FolderRuntime {
    operation_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    generations: Arc<Mutex<HashMap<String, u64>>>,
    open_editors: Arc<Mutex<HashSet<String>>>,
    folder_transition: Arc<Mutex<()>>,
}

#[derive(Debug, Default)]
pub struct ApplicationServices {
    folders: Mutex<HashMap<PathBuf, Weak<FolderRuntime>>>,
}

static DEFAULT_APPLICATION_SERVICES: OnceLock<Arc<ApplicationServices>> = OnceLock::new();

impl ApplicationServices {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn shared() -> Arc<Self> {
        Arc::clone(DEFAULT_APPLICATION_SERVICES.get_or_init(Self::new))
    }

    fn runtime(&self, root: &Path) -> Arc<FolderRuntime> {
        let mut folders = self
            .folders
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(runtime) = folders.get(root).and_then(Weak::upgrade) {
            return runtime;
        }
        let runtime = Arc::new(FolderRuntime::default());
        folders.insert(root.to_path_buf(), Arc::downgrade(&runtime));
        runtime
    }
}

pub struct NoteFolder {
    _runtime: Arc<FolderRuntime>,
    root: PathBuf,
    state_dir: PathBuf,
    index: Connection,
    state: Connection,
    adopted: bool,
    context_id: String,
    operation_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    generations: Arc<Mutex<HashMap<String, u64>>>,
    open_editors: Arc<Mutex<HashSet<String>>>,
    folder_transition: Arc<Mutex<()>>,
}

pub struct NoteEditor {
    _runtime: Arc<FolderRuntime>,
    editor: EditorSession,
    note_id: Identity,
    relative_path: PathBuf,
    absolute_path: PathBuf,
    recovery_path: PathBuf,
    pending_native_draft_path: PathBuf,
    base_hash: String,
    dirty: bool,
    owner_context_id: String,
    generation: u64,
    generations: Arc<Mutex<HashMap<String, u64>>>,
    operation_lock: Arc<Mutex<()>>,
    open_editors: Arc<Mutex<HashSet<String>>>,
}

impl Drop for NoteEditor {
    fn drop(&mut self) {
        self.open_editors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(self.note_id.as_str());
    }
}

impl NoteFolder {
    pub fn create(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
    ) -> Result<Self, AppError> {
        let services = ApplicationServices::shared();
        Self::create_with_services(root, application_state_root, adopt, &services)
    }

    fn create_with_services(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
        services: &ApplicationServices,
    ) -> Result<Self, AppError> {
        fs::create_dir_all(root.as_ref())?;
        Self::open_with_services(root, application_state_root, adopt, services)
    }

    pub fn open(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
    ) -> Result<Self, AppError> {
        let services = ApplicationServices::shared();
        Self::open_with_services(root, application_state_root, adopt, &services)
    }

    fn open_with_services(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
        services: &ApplicationServices,
    ) -> Result<Self, AppError> {
        let root = fs::canonicalize(root.as_ref())?;
        let runtime = services.runtime(&root);
        let _folder_transition = if adopt {
            Some(
                runtime
                    .folder_transition
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        } else {
            None
        };
        if adopt {
            let open_editors = runtime
                .open_editors
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(id) = open_editors.iter().next() {
                return Err(AppError::NoteAlreadyOpen(id.clone()));
            }
        }
        if !root.is_dir() {
            return Err(AppError::Io(io::Error::other(
                "note folder is not a directory",
            )));
        }
        fs::create_dir_all(application_state_root.as_ref())?;
        let state_root = fs::canonicalize(application_state_root.as_ref())?;
        let existing_library = read_library_id(&root)?;
        let library_id = match (existing_library, adopt) {
            (Some(id), true) => {
                let provisional = state_root
                    .join("folders")
                    .join(provisional_folder_id(&root));
                let manifest = load_or_create_adoption_manifest(&root, &provisional, Some(&id))?;
                ensure_no_pending_native_drafts_for_adoption(&[
                    provisional.join("pending-native"),
                    state_root.join("folders").join(&id).join("pending-native"),
                ])?;
                apply_adoption_manifest(&root, &manifest, &runtime.generations)?;
                let adopted = state_root.join("folders").join(&id);
                migrate_provisional_state(&provisional, &adopted, &manifest.mappings)?;
                complete_adoption_manifest(&provisional)?;
                Some(id)
            }
            (Some(id), false) => Some(id),
            (None, true) => {
                let provisional = state_root
                    .join("folders")
                    .join(provisional_folder_id(&root));
                let manifest = load_or_create_adoption_manifest(&root, &provisional, None)?;
                ensure_no_pending_native_drafts_for_adoption(&[
                    provisional.join("pending-native"),
                    state_root
                        .join("folders")
                        .join(&manifest.library_id)
                        .join("pending-native"),
                ])?;
                apply_adoption_manifest(&root, &manifest, &runtime.generations)?;
                let adopted = state_root.join("folders").join(&manifest.library_id);
                migrate_provisional_state(&provisional, &adopted, &manifest.mappings)?;
                // Metadata is published only after note and local-state migration completes.
                write_library(&root, &manifest.library_id)?;
                complete_adoption_manifest(&provisional)?;
                Some(manifest.library_id)
            }
            (None, false) => None,
        };
        let state_id = library_id
            .clone()
            .unwrap_or_else(|| provisional_folder_id(&root));
        let state_dir = state_root.join("folders").join(state_id);
        fs::create_dir_all(state_dir.join("recovery"))?;
        fs::create_dir_all(state_dir.join("pending-native"))?;
        fs::create_dir_all(state_dir.join("operations"))?;
        let index = Connection::open(state_dir.join("index.sqlite3"))?;
        initialize_index(&index)?;
        let state = Connection::open(state_dir.join("state.sqlite3"))?;
        initialize_state(&state)?;
        let folder = Self {
            _runtime: Arc::clone(&runtime),
            root,
            state_dir,
            index,
            state,
            adopted: library_id.is_some(),
            context_id: Ulid::new().to_string(),
            operation_locks: Arc::clone(&runtime.operation_locks),
            generations: Arc::clone(&runtime.generations),
            open_editors: Arc::clone(&runtime.open_editors),
            folder_transition: Arc::clone(&runtime.folder_transition),
        };
        folder.rebuild_index()?;
        Ok(folder)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn context_id(&self) -> &str {
        &self.context_id
    }

    pub fn is_adopted(&self) -> bool {
        self.adopted
    }

    pub fn discover(&self) -> Result<Vec<NoteSummary>, AppError> {
        discover_notes(&self.root)
    }

    pub fn create_note(&self, initial_source: Option<&str>) -> Result<NoteSummary, AppError> {
        let id = Ulid::new().to_string();
        let source = if self.adopted {
            set_adopted_id(initial_source.unwrap_or(""), &id)
        } else {
            initial_source.unwrap_or("").to_owned()
        };
        let relative = PathBuf::from(format!("untitled-{}.md", id.to_lowercase()));
        let destination = self.resolve_for_mutation(&relative, true)?;
        if destination.exists() {
            return Err(AppError::DestinationExists(relative));
        }
        self.revalidate_for_mutation(&relative, true)?;
        create_no_replace(&destination, source.as_bytes())?;
        let note = self.summary_from_source(&relative, &source)?;
        self.index_note(&note, &source)?;
        self.record_recent(&note.relative_path)?;
        Ok(note)
    }

    fn create_note_idempotent(
        &self,
        initial_source: &str,
        operation_id: &str,
        request_key: &str,
    ) -> Result<NoteSummary, AppError> {
        validate_operation_id(operation_id)?;
        let lock = self.note_lock(&format!("operation:{operation_id}"));
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let record_path = self
            .state_dir
            .join("operations")
            .join(format!("{}.json", safe_key(operation_id)));
        let (record, source) = if record_path.exists() {
            let record: NoteCreationRecord = serde_json::from_slice(&fs::read(&record_path)?)
                .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
            if record.operation_id != operation_id {
                return Err(AppError::MalformedMetadata(
                    "note-creation operation identity collision".into(),
                ));
            }
            if record.request_hash != hash(request_key) {
                return Err(AppError::MalformedMetadata(
                    "note-creation operation was retried with a different request".into(),
                ));
            }
            let source = if self.adopted {
                set_adopted_id(initial_source, record.note.id.as_str())
            } else {
                initial_source.to_owned()
            };
            if hash(&source) != record.source_hash {
                return Err(AppError::MalformedMetadata(
                    "note-creation operation was retried with different source".into(),
                ));
            }
            (record, source)
        } else {
            let seed = Ulid::new().to_string();
            let relative_path = PathBuf::from(format!("untitled-{}.md", seed.to_lowercase()));
            let source = if self.adopted {
                set_adopted_id(initial_source, &seed)
            } else {
                initial_source.to_owned()
            };
            let note = self.summary_from_source(&relative_path, &source)?;
            let record = NoteCreationRecord {
                operation_id: operation_id.into(),
                note,
                request_hash: hash(request_key),
                source_hash: hash(&source),
            };
            atomic_write(
                &record_path,
                &serde_json::to_vec(&record).map_err(io::Error::other)?,
            )?;
            (record, source)
        };
        let destination = self.resolve_for_mutation(&record.note.relative_path, true)?;
        if destination.exists() {
            let existing = read_utf8(&destination, &record.note.relative_path)?;
            if existing != source {
                return Err(AppError::DestinationExists(record.note.relative_path));
            }
        } else if let Err(error) = create_no_replace(&destination, source.as_bytes()) {
            let committed = destination.exists()
                && read_utf8(&destination, &record.note.relative_path)
                    .is_ok_and(|value| value == source);
            if !committed {
                return Err(error);
            }
        }
        let note = self.summary_from_source(&record.note.relative_path, &source)?;
        if note.id != record.note.id {
            return Err(AppError::IdentityChanged);
        }
        if let Err(error) = self.index_note(&note, &source) {
            let _ = self.mark_index_stale(&note.relative_path, &error.to_string());
        }
        let _ = self.record_recent(&note.relative_path);
        Ok(note)
    }

    fn note_created_by_operation(
        &self,
        operation_id: &str,
        request_key: &str,
    ) -> Result<Option<NoteSummary>, AppError> {
        validate_operation_id(operation_id)?;
        let record_path = self
            .state_dir
            .join("operations")
            .join(format!("{}.json", safe_key(operation_id)));
        if !record_path.exists() {
            return Ok(None);
        }
        let record: NoteCreationRecord = serde_json::from_slice(&fs::read(record_path)?)
            .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
        if record.operation_id != operation_id || record.request_hash != hash(request_key) {
            return Err(AppError::MalformedMetadata(
                "note-creation operation was retried with a different request".into(),
            ));
        }
        Ok(Some(record.note))
    }

    pub fn open_editor(&self, id: &str) -> Result<NoteEditor, AppError> {
        self.open_editor_with_pending(id, false)
    }

    fn open_editor_with_pending(
        &self,
        id: &str,
        allow_pending_native_draft: bool,
    ) -> Result<NoteEditor, AppError> {
        let transition = Arc::clone(&self.folder_transition);
        let _transition = transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !allow_pending_native_draft {
            self.ensure_no_pending_native_draft(id)?;
        }
        if self.recoveries()?.iter().any(|draft| draft.note_id == id) {
            return Err(AppError::RecoveryPending(id.into()));
        }
        let note = self.find_unique_note(id)?;
        self.editor_for_note(note, None)
    }

    pub fn restore_recovery(&self, id: &str) -> Result<NoteEditor, AppError> {
        self.restore_recovery_with_pending(id, false)
    }

    fn restore_recovery_with_pending(
        &self,
        id: &str,
        allow_pending_native_draft: bool,
    ) -> Result<NoteEditor, AppError> {
        let transition = Arc::clone(&self.folder_transition);
        let _transition = transition
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !allow_pending_native_draft {
            self.ensure_no_pending_native_draft(id)?;
        }
        let draft = self
            .recoveries()?
            .into_iter()
            .find(|draft| draft.note_id == id)
            .ok_or_else(|| AppError::RecoveryNotFound(id.into()))?;
        validate_identity(&draft.note_id)?;
        let source_path = self.resolve_existing(&draft.relative_path)?;
        let disk_source = read_utf8(&source_path, &draft.relative_path)?;
        let identity = if draft.note_id.len() == 26 {
            Identity::Adopted(draft.note_id.clone())
        } else {
            Identity::Provisional(draft.note_id.clone())
        };
        let note = NoteSummary {
            id: identity,
            relative_path: draft.relative_path.clone(),
            title: derive_title(&disk_source),
            content_hash: hash(&disk_source),
        };
        self.editor_for_note(note, Some(draft))
    }

    pub fn discard_recovery(&self, id: &str) -> Result<(), AppError> {
        validate_identity(id)?;
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = self.recovery_path(id);
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(AppError::RecoveryNotFound(id.into()))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn save_editor(
        &self,
        editor: &mut NoteEditor,
        expected_revision: u64,
    ) -> Result<SaveOutcome, AppError> {
        self.ensure_owner(editor)?;
        let lock = self.note_lock(editor.note_id.as_str());
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let actual = editor.snapshot().revision;
        if expected_revision != actual {
            return Err(EditorError::StaleRevision {
                expected: expected_revision,
                actual,
            }
            .into());
        }
        self.save_editor_locked(editor, || {}, sync_parent)
    }

    #[cfg(test)]
    fn save_editor_with_hook(
        &self,
        editor: &mut NoteEditor,
        hook: impl FnOnce(),
    ) -> Result<SaveOutcome, AppError> {
        self.ensure_owner(editor)?;
        let lock = self.note_lock(editor.note_id.as_str());
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.save_editor_locked(editor, hook, sync_parent)
    }

    #[cfg(test)]
    fn save_editor_with_sync_failure(
        &self,
        editor: &mut NoteEditor,
    ) -> Result<SaveOutcome, AppError> {
        self.ensure_owner(editor)?;
        let lock = self.note_lock(editor.note_id.as_str());
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.save_editor_locked(
            editor,
            || {},
            |_| Err(AppError::Io(io::Error::other("injected sync failure"))),
        )
    }

    fn save_editor_locked(
        &self,
        editor: &mut NoteEditor,
        before_second_validation: impl FnOnce(),
        sync: impl FnOnce(&Path) -> Result<(), AppError>,
    ) -> Result<SaveOutcome, AppError> {
        editor.ensure_current_generation()?;
        self.revalidate_for_mutation(&editor.relative_path, false)?;
        let snapshot = editor.editor.snapshot();
        ensure_same_identity(&editor.note_id, &snapshot.source)?;
        let recovery_error = editor
            .persist_recovery()
            .err()
            .map(|error| error.to_string());
        let current = read_utf8(&editor.absolute_path, &editor.relative_path)?;
        ensure_same_identity(&editor.note_id, &current)?;
        if hash(&current) != editor.base_hash {
            return Err(AppError::ExternalConflict {
                external_source: current,
            });
        }
        let parent = editor.absolute_path.parent().ok_or(AppError::PathEscape)?;
        let mut temporary = NamedTempFile::new_in(parent)?;
        temporary.write_all(snapshot.source.as_bytes())?;
        temporary.as_file_mut().sync_all()?;

        before_second_validation();
        editor.ensure_current_generation()?;
        self.revalidate_for_mutation(&editor.relative_path, false)?;
        let coordinated_current = read_utf8(&editor.absolute_path, &editor.relative_path)?;
        ensure_same_identity(&editor.note_id, &coordinated_current)?;
        if hash(&coordinated_current) != editor.base_hash {
            return Err(AppError::ExternalConflict {
                external_source: coordinated_current,
            });
        }
        // This application lock serializes Howler operations only. The validation and rename are
        // not a filesystem compare-and-swap; an external process can still race this small window.
        temporary
            .persist(&editor.absolute_path)
            .map_err(|error| error.error)?;
        if let Err(error) = sync(parent) {
            editor.base_hash = hash(&snapshot.source);
            editor.dirty = true;
            return Ok(SaveOutcome {
                revision: snapshot.revision,
                durability: if recovery_error.is_none() {
                    DurabilityState::RecoveryDurable
                } else {
                    DurabilityState::Accepted
                },
                recovery_cleanup: CleanupState::Retained,
                recovery_cleanup_error: recovery_error,
                index_state: IndexState::Stale,
                index_error: Some(
                    "canonical durability is uncertain; index update deferred".into(),
                ),
                recency_error: None,
                canonical_error: Some(format!(
                    "canonical file replaced but parent sync failed: {error}"
                )),
            });
        }
        editor.base_hash = hash(&snapshot.source);
        editor.dirty = false;

        let (index_state, index_error) = match self.index_note_values(
            &editor.note_id,
            &editor.relative_path,
            &snapshot.source,
            &editor.base_hash,
            &editor.absolute_path,
        ) {
            Ok(()) => match self.clear_stale_index(&editor.relative_path) {
                Ok(()) => (IndexState::Current, None),
                Err(error) => (IndexState::Stale, Some(error.to_string())),
            },
            Err(error) => {
                let mut message = error.to_string();
                if let Err(stale_error) = self.mark_index_stale(&editor.relative_path, &message) {
                    message = format!("{message}; stale marker failed: {stale_error}");
                }
                (IndexState::Stale, Some(message))
            }
        };
        let (recovery_cleanup, recovery_cleanup_error) =
            match fs::remove_file(&editor.recovery_path) {
                Ok(()) => (CleanupState::Removed, recovery_error),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    (CleanupState::AlreadyAbsent, recovery_error)
                }
                Err(error) => (CleanupState::Retained, Some(error.to_string())),
            };
        let recency_error = self
            .record_recent(&editor.relative_path)
            .err()
            .map(|error| error.to_string());
        Ok(SaveOutcome {
            revision: snapshot.revision,
            durability: DurabilityState::FileSaved,
            recovery_cleanup,
            recovery_cleanup_error,
            index_state,
            index_error,
            recency_error,
            canonical_error: None,
        })
    }

    pub fn rename_title(&self, id: &str, title: &str) -> Result<NoteSummary, AppError> {
        if title.contains(['\r', '\n']) {
            return Err(AppError::InvalidTitle);
        }
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_native_draft(id)?;
        self.ensure_no_pending_recovery(id)?;
        let note = self.find_unique_note(id)?;
        let mut editor = self.editor_for_note(note, None)?;
        let (summary, _) = self.rename_editor_title_locked(&mut editor, title)?;
        Ok(summary)
    }

    fn rename_editor_title(
        &self,
        editor: &mut NoteEditor,
        title: &str,
    ) -> Result<(NoteSummary, SaveOutcome), AppError> {
        if title.contains(['\r', '\n']) {
            return Err(AppError::InvalidTitle);
        }
        self.ensure_owner(editor)?;
        let lock = self.note_lock(editor.note_id.as_str());
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_native_draft(editor.note_id.as_str())?;
        self.rename_editor_title_locked(editor, title)
    }

    fn rename_editor_title_locked(
        &self,
        editor: &mut NoteEditor,
        title: &str,
    ) -> Result<(NoteSummary, SaveOutcome), AppError> {
        editor.ensure_current_generation()?;
        let source = editor.editor.snapshot().source;
        let replacement = rename_title_source(&source, title)?;
        editor.editor.replace_external(&replacement);
        editor.dirty = true;
        let _ = editor.persist_recovery();
        let save = self.save_editor_locked(editor, || {}, sync_parent)?;
        Ok((self.summary_for_path(&editor.relative_path)?, save))
    }

    pub fn move_note(
        &self,
        id: &str,
        destination: impl AsRef<Path>,
    ) -> Result<NoteSummary, AppError> {
        let note = self.find_unique_note(id)?;
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_native_draft(id)?;
        self.ensure_no_pending_recovery(id)?;
        self.move_note_locked(&note, destination.as_ref())
            .map(|(summary, _)| summary)
    }

    fn move_editor(
        &self,
        editor: &mut NoteEditor,
        destination: &Path,
    ) -> Result<NoteSummary, AppError> {
        self.ensure_owner(editor)?;
        let old_id = editor.note_id.as_str().to_owned();
        let lock = self.note_lock(&old_id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_native_draft(&old_id)?;
        editor.ensure_current_generation()?;
        let note = NoteSummary {
            id: editor.note_id.clone(),
            relative_path: editor.relative_path.clone(),
            title: derive_title(&editor.editor.snapshot().source),
            content_hash: hash(&editor.editor.snapshot().source),
        };
        let mut open_editors = self
            .open_editors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (summary, absolute_path) = self.move_note_locked(&note, destination)?;
        let new_id = summary.id.as_str().to_owned();
        open_editors.remove(&old_id);
        let inserted = open_editors.insert(new_id);
        debug_assert!(inserted);
        drop(open_editors);
        editor.note_id = summary.id.clone();
        editor.relative_path = summary.relative_path.clone();
        editor.absolute_path = absolute_path;
        editor.recovery_path = self.recovery_path(summary.id.as_str());
        editor.pending_native_draft_path = self.pending_native_draft_path(summary.id.as_str());
        editor.generation = self.generation(summary.id.as_str());
        editor.operation_lock = self.note_lock(summary.id.as_str());
        Ok(summary)
    }

    fn move_note_locked(
        &self,
        note: &NoteSummary,
        destination: &Path,
    ) -> Result<(NoteSummary, PathBuf), AppError> {
        let from = self.resolve_existing(&note.relative_path)?;
        let source = read_utf8(&from, &note.relative_path)?;
        let destination = normalized_relative(destination)?;
        if !is_markdown(&destination) {
            return Err(AppError::PathEscape);
        }
        let summary = self.summary_from_source(&destination, &source)?;
        let to = self.resolve_for_mutation(&destination, true)?;
        if to.exists() {
            return Err(AppError::DestinationExists(destination));
        }
        self.create_safe_parents(&destination)?;
        self.revalidate_for_mutation(&note.relative_path, false)?;
        self.revalidate_for_mutation(&destination, true)?;
        move_no_replace(&from, &to).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                AppError::DestinationExists(destination.clone())
            } else {
                error.into()
            }
        })?;
        self.bump_generation(note.id.as_str());
        let _ = self.state.execute(
            "UPDATE recent_notes SET path=?1 WHERE path=?2",
            params![
                destination.to_string_lossy(),
                note.relative_path.to_string_lossy()
            ],
        );
        if let Err(error) = self.rebuild_index() {
            let _ = self.mark_index_stale(&destination, &error.to_string());
        }
        Ok((summary, to))
    }

    pub fn trash(&self, id: &str) -> Result<PathBuf, AppError> {
        let note = self.find_unique_note(id)?;
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_native_draft(id)?;
        self.ensure_no_pending_recovery(id)?;
        let source = self.resolve_existing(&note.relative_path)?;
        self.create_safe_parents(Path::new(".trash/placeholder"))?;
        let trash = self.resolve_for_mutation(Path::new(".trash"), false)?;
        let file_name = note.relative_path.file_name().ok_or(AppError::PathEscape)?;
        self.revalidate_for_mutation(&note.relative_path, false)?;
        self.revalidate_for_mutation(Path::new(".trash"), false)?;
        let mut destination = trash.join(file_name);
        loop {
            match move_no_replace(&source, &destination) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    destination =
                        trash.join(format!("{}-{}", Ulid::new(), file_name.to_string_lossy()));
                }
                Err(error) => return Err(error.into()),
            }
        }
        self.bump_generation(id);
        if let Err(error) = self.rebuild_index() {
            let _ = self.mark_index_stale(&note.relative_path, &error.to_string());
        }
        Ok(destination.strip_prefix(&self.root).unwrap().to_path_buf())
    }

    pub fn restore(
        &self,
        trashed_relative_path: impl AsRef<Path>,
    ) -> Result<NoteSummary, AppError> {
        let relative = normalized_relative(trashed_relative_path.as_ref())?;
        if relative.components().next() != Some(Component::Normal(".trash".as_ref())) {
            return Err(AppError::PathEscape);
        }
        let source = self.resolve_existing(&relative)?;
        let file_name = source.file_name().ok_or(AppError::PathEscape)?.to_owned();
        self.revalidate_for_mutation(&relative, false)?;
        let mut destination = self.root.join(&file_name);
        loop {
            let destination_relative = destination
                .strip_prefix(&self.root)
                .map_err(|_| AppError::PathEscape)?;
            self.revalidate_for_mutation(destination_relative, true)?;
            match move_no_replace(&source, &destination) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    destination = self.root.join(format!(
                        "restored-{}-{}",
                        Ulid::new(),
                        file_name.to_string_lossy()
                    ));
                }
                Err(error) => return Err(error.into()),
            }
        }
        let relative = destination.strip_prefix(&self.root).unwrap();
        let summary = self.summary_for_path(relative)?;
        if let Err(error) = self.rebuild_index() {
            let _ = self.mark_index_stale(relative, &error.to_string());
        }
        Ok(summary)
    }

    pub fn recoveries(&self) -> Result<Vec<RecoveryDraft>, AppError> {
        let mut drafts = Vec::new();
        for entry in fs::read_dir(self.state_dir.join("recovery"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            if let Ok(draft) = serde_json::from_slice::<RecoveryDraft>(&bytes) {
                validate_identity(&draft.note_id)?;
                normalized_relative(&draft.relative_path)?;
                drafts.push(draft);
            }
        }
        drafts.sort_by(|left: &RecoveryDraft, right| left.note_id.cmp(&right.note_id));
        Ok(drafts)
    }

    fn pending_native_drafts(&self) -> Result<Vec<PendingNativeDraftRecord>, AppError> {
        let mut drafts = Vec::new();
        for entry in fs::read_dir(self.state_dir.join("pending-native"))? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let draft: PendingNativeDraftRecord = serde_json::from_slice(&fs::read(path)?)
                .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
            validate_identity(&draft.note_id)?;
            normalized_relative(&draft.relative_path)?;
            drafts.push(draft);
        }
        drafts.sort_by(|left, right| left.note_id.cmp(&right.note_id));
        Ok(drafts)
    }

    pub fn rebuild_index(&self) -> Result<RebuildReport, AppError> {
        let mut values = Vec::new();
        let mut report = RebuildReport {
            indexed: 0,
            skipped: 0,
        };
        for entry in markdown_entries(&self.root) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            };
            let relative = match entry.path().strip_prefix(&self.root) {
                Ok(path) => path.to_path_buf(),
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            };
            let source = match read_utf8(entry.path(), &relative) {
                Ok(source) => source,
                Err(_) => {
                    report.skipped += 1;
                    continue;
                }
            };
            let note = self.summary_from_source(&relative, &source)?;
            values.push(index_values(
                &note.id,
                &relative,
                &source,
                &note.content_hash,
                entry.path(),
            )?);
            report.indexed += 1;
        }
        let transaction = self.index.unchecked_transaction()?;
        transaction.execute("DELETE FROM note_fts", [])?;
        transaction.execute("DELETE FROM notes", [])?;
        for value in &values {
            insert_index_value(&transaction, value)?;
        }
        transaction.commit()?;
        self.state.execute("DELETE FROM stale_index", [])?;
        Ok(report)
    }

    pub fn retry_stale_indexes(&self) -> Result<usize, AppError> {
        let paths = {
            let mut statement = self
                .state
                .prepare("SELECT path FROM stale_index ORDER BY path")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut repaired = 0;
        for value in paths {
            let relative = PathBuf::from(value);
            let absolute = self.resolve(&relative)?;
            let source = match read_utf8(&absolute, &relative) {
                Ok(source) => source,
                Err(_) => continue,
            };
            let note = self.summary_from_source(&relative, &source)?;
            if self.index_note(&note, &source).is_ok() {
                self.clear_stale_index(&relative)?;
                repaired += 1;
            }
        }
        Ok(repaired)
    }

    pub fn rescan(&self) -> Result<RescanReport, AppError> {
        let notes = self.discover()?.len();
        let rebuild = self.rebuild_index()?;
        let diagnostics = self.diagnostics()?;
        Ok(RescanReport {
            notes,
            rebuild,
            diagnostics,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, AppError> {
        self.retry_stale_indexes()?;
        let recent = self.recent_paths()?;
        let recent_rank: HashMap<_, _> = recent
            .iter()
            .enumerate()
            .map(|(rank, path)| (path.clone(), rank))
            .collect();
        let fts_matches = if query.trim().is_empty() {
            HashSet::new()
        } else {
            self.fts_matching_paths(query)?
        };
        let mut statement = self
            .index
            .prepare("SELECT id, adopted, path, title, body, hash FROM notes")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let query_lower = query.trim().to_lowercase();
        let mut ranked = Vec::new();
        for row in rows {
            let (id, adopted, path, title, body, content_hash) = row?;
            let title_lower = title.to_lowercase();
            let reason = if query_lower.is_empty() {
                MatchReason::Recent
            } else if title_lower == query_lower {
                MatchReason::ExactTitle
            } else if title_lower.starts_with(&query_lower) {
                MatchReason::PrefixTitle
            } else if title_lower.contains(&query_lower)
                || fuzzy_subsequence(&title_lower, &query_lower)
            {
                MatchReason::FuzzyTitle
            } else if fts_matches.contains(&path) {
                MatchReason::Body
            } else {
                continue;
            };
            let reason_rank = match reason {
                MatchReason::ExactTitle => 0,
                MatchReason::PrefixTitle => 1,
                MatchReason::FuzzyTitle => 2,
                MatchReason::Body => 3,
                MatchReason::Recent => 4,
            };
            let recency = recent_rank.get(&path).copied().unwrap_or(usize::MAX);
            ranked.push((
                reason_rank,
                recency,
                path.clone(),
                SearchResult {
                    note: NoteSummary {
                        id: if adopted {
                            Identity::Adopted(id)
                        } else {
                            Identity::Provisional(id)
                        },
                        relative_path: PathBuf::from(path),
                        title,
                        content_hash,
                    },
                    snippet: body.chars().take(180).collect(),
                    reason,
                },
            ));
        }
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.cmp(&right.2))
        });
        Ok(ranked
            .into_iter()
            .take(limit)
            .map(|(_, _, _, result)| result)
            .collect())
    }

    pub fn diagnostics(&self) -> Result<Vec<Diagnostic>, AppError> {
        let mut diagnostics = Vec::new();
        let mut ids: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut folded_paths = HashSet::new();
        for entry in markdown_entries(&self.root) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        severity: "error".into(),
                        code: "walk_error".into(),
                        relative_path: None,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let relative = entry.path().strip_prefix(&self.root).unwrap().to_path_buf();
            let folded = relative.to_string_lossy().to_lowercase();
            if !folded_paths.insert(folded) {
                diagnostics.push(Diagnostic {
                    severity: "warning".into(),
                    code: "case_collision".into(),
                    relative_path: Some(relative.clone()),
                    message: "path collides when case-folded".into(),
                });
            }
            let source = match read_utf8(entry.path(), &relative) {
                Ok(source) => source,
                Err(error) => {
                    diagnostics.push(Diagnostic {
                        severity: "error".into(),
                        code: "invalid_utf8".into(),
                        relative_path: Some(relative),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let parsed = metadata(&source);
            if let Some(error) = parsed.error {
                diagnostics.push(Diagnostic {
                    severity: "warning".into(),
                    code: "malformed_front_matter".into(),
                    relative_path: Some(relative.clone()),
                    message: error,
                });
            }
            if let Some(id) = parsed.id {
                if validate_adopted_identity(&id).is_err() {
                    diagnostics.push(Diagnostic {
                        severity: "error".into(),
                        code: "invalid_note_id".into(),
                        relative_path: Some(relative),
                        message: "howler_id must be a ULID".into(),
                    });
                } else {
                    ids.entry(id).or_default().push(relative);
                }
            }
        }
        for (id, paths) in ids {
            if paths.len() > 1 {
                for path in paths {
                    diagnostics.push(Diagnostic {
                        severity: "error".into(),
                        code: "duplicate_note_id".into(),
                        relative_path: Some(path),
                        message: format!("duplicate howler_id {id}"),
                    });
                }
            }
        }
        let mut statement = self
            .state
            .prepare("SELECT path FROM stale_index ORDER BY path")?;
        let stale_paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for path in stale_paths {
            diagnostics.push(Diagnostic {
                severity: "warning".into(),
                code: "stale_index".into(),
                relative_path: Some(PathBuf::from(path)),
                message: "index update pending".into(),
            });
        }
        diagnostics.sort_by(|left, right| {
            left.code
                .cmp(&right.code)
                .then(left.relative_path.cmp(&right.relative_path))
        });
        Ok(diagnostics)
    }

    pub fn diagnostic_bundle(&self) -> Result<DiagnosticBundle, AppError> {
        Ok(DiagnosticBundle {
            application_version: APPLICATION_VERSION.into(),
            editor_version: howler_editor::EDITOR_VERSION.into(),
            index_schema: INDEX_SCHEMA_VERSION,
            state_schema: STATE_SCHEMA_VERSION,
            adopted: self.adopted,
            note_count: self.discover()?.len(),
            recovery_count: self.recoveries()?.len(),
            diagnostics: self.diagnostics()?,
        })
    }

    pub fn set_cursor(&self, relative_path: &Path, byte_offset: usize) -> Result<(), AppError> {
        let relative = normalized_relative(relative_path)?;
        self.state.execute(
            "INSERT INTO cursor_state(path, byte_offset) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET byte_offset=excluded.byte_offset",
            params![relative.to_string_lossy(), byte_offset as i64],
        )?;
        Ok(())
    }

    pub fn cursor(&self, relative_path: &Path) -> Result<Option<usize>, AppError> {
        let relative = normalized_relative(relative_path)?;
        let mut statement = self
            .state
            .prepare("SELECT byte_offset FROM cursor_state WHERE path=?1")?;
        let mut rows = statement.query(params![relative.to_string_lossy()])?;
        Ok(rows
            .next()?
            .map(|row| row.get::<_, i64>(0))
            .transpose()?
            .map(|value| value as usize))
    }

    fn editor_for_note(
        &self,
        note: NoteSummary,
        recovery: Option<RecoveryDraft>,
    ) -> Result<NoteEditor, AppError> {
        let absolute_path = self.resolve_existing(&note.relative_path)?;
        let disk_source = read_utf8(&absolute_path, &note.relative_path)?;
        let mut editor = EditorSession::new(&disk_source);
        let dirty = recovery.is_some();
        let base_hash = recovery
            .as_ref()
            .map(|draft| draft.base_hash.clone())
            .unwrap_or_else(|| hash(&disk_source));
        if let Some(draft) = recovery {
            editor.replace_external(&draft.source);
        }
        self.record_recent(&note.relative_path)?;
        let generation = self.generation(note.id.as_str());
        let operation_lock = self.note_lock(note.id.as_str());
        let mut open_editors = self
            .open_editors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !open_editors.insert(note.id.as_str().to_owned()) {
            return Err(AppError::NoteAlreadyOpen(note.id.as_str().into()));
        }
        drop(open_editors);
        Ok(NoteEditor {
            _runtime: Arc::clone(&self._runtime),
            editor,
            recovery_path: self.recovery_path(note.id.as_str()),
            pending_native_draft_path: self.pending_native_draft_path(note.id.as_str()),
            note_id: note.id,
            relative_path: note.relative_path,
            absolute_path,
            base_hash,
            dirty,
            owner_context_id: self.context_id.clone(),
            generation,
            generations: Arc::clone(&self.generations),
            operation_lock,
            open_editors: Arc::clone(&self.open_editors),
        })
    }

    fn find_unique_note(&self, id: &str) -> Result<NoteSummary, AppError> {
        let mut matches = self
            .discover()?
            .into_iter()
            .filter(|note| note.id.as_str() == id);
        let note = matches
            .next()
            .ok_or_else(|| AppError::NoteNotFound(id.into()))?;
        if matches.next().is_some() {
            return Err(AppError::DuplicateIdentity(id.into()));
        }
        Ok(note)
    }

    fn ensure_owner(&self, editor: &NoteEditor) -> Result<(), AppError> {
        if editor.owner_context_id != self.context_id {
            return Err(AppError::WrongOwner);
        }
        Ok(())
    }

    fn resolve(&self, relative: &Path) -> Result<PathBuf, AppError> {
        Ok(self.root.join(normalized_relative(relative)?))
    }

    fn resolve_existing(&self, relative: &Path) -> Result<PathBuf, AppError> {
        self.revalidate_for_mutation(relative, false)
    }

    fn resolve_for_mutation(
        &self,
        relative: &Path,
        leaf_may_be_missing: bool,
    ) -> Result<PathBuf, AppError> {
        validate_no_symlink_components(&self.root, relative, leaf_may_be_missing)
    }

    fn revalidate_for_mutation(
        &self,
        relative: &Path,
        leaf_may_be_missing: bool,
    ) -> Result<PathBuf, AppError> {
        validate_no_symlink_components(&self.root, relative, leaf_may_be_missing)
    }

    fn create_safe_parents(&self, relative: &Path) -> Result<(), AppError> {
        let relative = normalized_relative(relative)?;
        let mut current = self.root.clone();
        if let Some(parent) = relative.parent() {
            for component in parent.components() {
                let Component::Normal(component) = component else {
                    return Err(AppError::PathEscape);
                };
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(AppError::PathEscape)
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        fs::create_dir(&current)?;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    fn summary_for_path(&self, relative: &Path) -> Result<NoteSummary, AppError> {
        let source = read_utf8(&self.resolve(relative)?, relative)?;
        self.summary_from_source(relative, &source)
    }

    fn summary_from_source(&self, relative: &Path, source: &str) -> Result<NoteSummary, AppError> {
        let content_hash = hash(source);
        let id = identity_for_source(relative, source)?;
        Ok(NoteSummary {
            id,
            relative_path: relative.to_path_buf(),
            title: derive_title(source),
            content_hash,
        })
    }

    fn note_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.operation_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn ensure_no_pending_recovery(&self, id: &str) -> Result<(), AppError> {
        if self.recoveries()?.iter().any(|draft| draft.note_id == id) {
            Err(AppError::RecoveryPending(id.into()))
        } else {
            Ok(())
        }
    }

    fn ensure_no_pending_native_draft(&self, id: &str) -> Result<(), AppError> {
        if self
            .pending_native_drafts()?
            .iter()
            .any(|draft| draft.note_id == id)
        {
            Err(AppError::PendingNativeDraft(id.into()))
        } else {
            Ok(())
        }
    }

    fn recovery_path(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("recovery")
            .join(format!("{}.json", safe_key(id)))
    }

    fn pending_native_draft_path(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("pending-native")
            .join(format!("{}.json", safe_key(id)))
    }

    fn generation(&self, id: &str) -> u64 {
        *self
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .unwrap_or(&0)
    }

    fn bump_generation(&self, id: &str) {
        let mut generations = self
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *generations.entry(id.to_owned()).or_default() += 1;
    }

    fn index_note(&self, note: &NoteSummary, source: &str) -> Result<(), AppError> {
        self.index_note_values(
            &note.id,
            &note.relative_path,
            source,
            &note.content_hash,
            &self.resolve(&note.relative_path)?,
        )
    }

    fn index_note_values(
        &self,
        id: &Identity,
        relative: &Path,
        source: &str,
        content_hash: &str,
        absolute: &Path,
    ) -> Result<(), AppError> {
        let value = index_values(id, relative, source, content_hash, absolute)?;
        let transaction = self.index.unchecked_transaction()?;
        insert_index_value(&transaction, &value)?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_index_stale(&self, relative: &Path, error: &str) -> Result<(), AppError> {
        self.state.execute(
            "INSERT INTO stale_index(path,error) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET error=excluded.error",
            params![relative.to_string_lossy(), error],
        )?;
        Ok(())
    }

    fn clear_stale_index(&self, relative: &Path) -> Result<(), AppError> {
        self.state.execute(
            "DELETE FROM stale_index WHERE path=?1",
            params![relative.to_string_lossy()],
        )?;
        Ok(())
    }

    fn record_recent(&self, relative: &Path) -> Result<(), AppError> {
        self.state.execute(
            "INSERT INTO sequence(name,value) VALUES('recent',1) ON CONFLICT(name) DO UPDATE SET value=value+1",
            [],
        )?;
        let sequence: i64 = self.state.query_row(
            "SELECT value FROM sequence WHERE name='recent'",
            [],
            |row| row.get(0),
        )?;
        self.state.execute(
            "INSERT INTO recent_notes(path,last_opened) VALUES(?1,?2) ON CONFLICT(path) DO UPDATE SET last_opened=excluded.last_opened",
            params![relative.to_string_lossy(), sequence],
        )?;
        Ok(())
    }

    fn recent_paths(&self) -> Result<Vec<String>, AppError> {
        let mut statement = self
            .state
            .prepare("SELECT path FROM recent_notes ORDER BY last_opened DESC, path")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn fts_matching_paths(&self, query: &str) -> Result<HashSet<String>, AppError> {
        let fts_query = query
            .split_whitespace()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut statement = self.index.prepare(
            "SELECT n.path FROM note_fts JOIN notes n ON n.rowid=note_fts.rowid WHERE note_fts MATCH ?1",
        )?;
        let rows = statement.query_map(params![fts_query], |row| row.get(0))?;
        Ok(rows.collect::<Result<HashSet<_>, _>>()?)
    }
}

impl NoteEditor {
    pub fn snapshot(&self) -> howler_editor::Snapshot {
        self.editor.snapshot()
    }

    pub fn note_id(&self) -> &Identity {
        &self.note_id
    }

    pub fn owner_context_id(&self) -> &str {
        &self.owner_context_id
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn persist_active_recovery(&self) -> Result<(), AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.persist_recovery()
    }

    fn persist_pending_native_draft(&self, draft: &PendingNativeDraft) -> Result<(), AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = PendingNativeDraftRecord {
            note_id: self.note_id.as_str().into(),
            relative_path: self.relative_path.clone(),
            base_revision: draft.base_revision,
            source: draft.source.clone(),
        };
        let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        atomic_write(&self.pending_native_draft_path, &bytes)
    }

    fn clear_pending_native_draft(&self) -> Result<(), AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match fs::remove_file(&self.pending_native_draft_path) {
            Ok(()) => sync_parent(
                self.pending_native_draft_path
                    .parent()
                    .ok_or(AppError::PathEscape)?,
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn install_external(&mut self, expected_hash: &str) -> Result<(), AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let external = read_utf8(&self.absolute_path, &self.relative_path)?;
        ensure_same_identity(&self.note_id, &external)?;
        let external_hash = hash(&external);
        if external_hash != expected_hash {
            return Err(AppError::ExternalConflict {
                external_source: external,
            });
        }
        match fs::remove_file(&self.recovery_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.editor.replace_external(&external);
        self.base_hash = external_hash;
        self.dirty = false;
        Ok(())
    }

    pub fn apply(&mut self, transaction: Transaction) -> Result<MutationOutcome, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let result = self.editor.apply(transaction)?;
        self.accepted(Some(result))
    }

    pub fn set_selections(
        &mut self,
        expected_revision: u64,
        selections: Vec<howler_editor::Selection>,
    ) -> Result<Vec<howler_editor::Selection>, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        Ok(self.editor.set_selections(expected_revision, selections)?)
    }

    pub fn execute_command(
        &mut self,
        expected_revision: u64,
        command: EditorCommand,
    ) -> Result<MutationOutcome, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let result = self.editor.execute_command(expected_revision, command)?;
        self.accepted(Some(result))
    }

    pub fn undo(&mut self, expected_revision: u64) -> Result<MutationOutcome, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let result = self.editor.undo(expected_revision)?;
        if result.is_none() {
            return Ok(MutationOutcome {
                edit: None,
                durability: if self.dirty {
                    DurabilityState::RecoveryDurable
                } else {
                    DurabilityState::FileSaved
                },
                recovery_error: None,
            });
        }
        self.accepted(result)
    }

    pub fn redo(&mut self, expected_revision: u64) -> Result<MutationOutcome, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let result = self.editor.redo(expected_revision)?;
        if result.is_none() {
            return Ok(MutationOutcome {
                edit: None,
                durability: if self.dirty {
                    DurabilityState::RecoveryDurable
                } else {
                    DurabilityState::FileSaved
                },
                recovery_error: None,
            });
        }
        self.accepted(result)
    }

    pub fn reconcile_external(&mut self) -> Result<ReconcileResult, AppError> {
        let lock = Arc::clone(&self.operation_lock);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_current_generation()?;
        let external = read_utf8(&self.absolute_path, &self.relative_path)?;
        ensure_same_identity(&self.note_id, &external)?;
        let external_hash = hash(&external);
        if external_hash == self.base_hash {
            return Ok(ReconcileResult::Unchanged);
        }
        if self.dirty {
            return Ok(ReconcileResult::Conflict {
                external_source: external,
            });
        }
        self.editor.replace_external(&external);
        self.base_hash = external_hash;
        Ok(ReconcileResult::Refreshed {
            revision: self.editor.snapshot().revision,
        })
    }

    fn accepted(&mut self, result: Option<EditResult>) -> Result<MutationOutcome, AppError> {
        self.dirty = true;
        match self.persist_recovery() {
            Ok(()) => Ok(MutationOutcome {
                edit: result,
                durability: DurabilityState::RecoveryDurable,
                recovery_error: None,
            }),
            Err(error) => Ok(MutationOutcome {
                edit: result,
                durability: DurabilityState::Accepted,
                recovery_error: Some(error.to_string()),
            }),
        }
    }

    fn persist_recovery(&self) -> Result<(), AppError> {
        let snapshot = self.editor.snapshot();
        let draft = RecoveryDraft {
            note_id: self.note_id.as_str().into(),
            relative_path: self.relative_path.clone(),
            revision: snapshot.revision,
            base_hash: self.base_hash.clone(),
            source: snapshot.source,
        };
        let bytes = serde_json::to_vec(&draft).map_err(io::Error::other)?;
        atomic_write(&self.recovery_path, &bytes)
    }

    fn ensure_current_generation(&self) -> Result<(), AppError> {
        let generation = *self
            .generations
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(self.note_id.as_str())
            .unwrap_or(&0);
        if generation != self.generation {
            return Err(AppError::StaleHandle);
        }
        Ok(())
    }
}

impl ApplicationServices {
    fn connect_folder(&self, request: &ConnectFolder) -> Result<NoteFolder, AppError> {
        if request.create_missing {
            NoteFolder::create_with_services(
                &request.folder_path,
                &request.application_state_path,
                request.adopt,
                self,
            )
        } else {
            NoteFolder::open_with_services(
                &request.folder_path,
                &request.application_state_path,
                request.adopt,
                self,
            )
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectFolder {
    pub folder_path: PathBuf,
    pub application_state_path: PathBuf,
    #[serde(default)]
    pub adopt: bool,
    #[serde(default)]
    pub create_missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNote {
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameNote {
    pub note_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveNote {
    pub note_id: String,
    pub destination: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreNote {
    pub trash_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

fn default_search_limit() -> usize {
    40
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositionCommit {
    pub original_range: howler_editor::TextRange,
    pub original_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputOrigin {
    Typing,
    Paste,
    Composition,
    Dictation,
    Autocorrection,
    Replacement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostTextEdit {
    pub expected_revision: u64,
    pub replacements: Vec<howler_editor::Replacement>,
    pub selections: Vec<howler_editor::Selection>,
    pub history: howler_editor::HistoryHint,
    #[serde(default)]
    pub composition: Option<CompositionCommit>,
    #[serde(default)]
    pub input_origin: Option<InputOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSelectionUpdate {
    pub expected_revision: u64,
    pub selections: Vec<howler_editor::Selection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    pub revision: u64,
    pub selections: Vec<howler_editor::Selection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCapabilities {
    pub application_session_abi: u32,
    pub selection_updates: bool,
    pub input_origin_metadata: bool,
    pub rust_owned_history: bool,
    pub pending_native_drafts: bool,
}

impl From<HostTextEdit> for Transaction {
    fn from(edit: HostTextEdit) -> Self {
        Self {
            expected_revision: edit.expected_revision,
            replacements: edit.replacements,
            selections: edit.selections,
            history: edit.history,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingNativeDraft {
    pub base_revision: u64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case")]
pub enum PendingDraftResolution {
    SaveAsNew {
        operation_id: String,
        title: Option<String>,
    },
    Discard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resolution", rename_all = "snake_case")]
pub enum ConflictResolution {
    UseExternal {
        expected_external_hash: String,
    },
    KeepLocalAsNewNote {
        operation_id: String,
        expected_external_hash: String,
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTarget {
    pub note_id: Identity,
    pub revision: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementSafety {
    Safe,
    MustRetainEditor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersistenceIssue {
    RecoveryWrite { diagnostic: String },
    CanonicalWrite { diagnostic: String },
    CanonicalDurabilityUncertain { diagnostic: String },
    RecoveryCleanup { diagnostic: String },
    IndexStale { diagnostic: String },
    RecencyUpdate { diagnostic: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceState {
    pub durability: DurabilityState,
    pub replacement_safety: ReplacementSafety,
    pub issues: Vec<PersistenceIssue>,
}

impl PersistenceState {
    fn saved() -> Self {
        Self {
            durability: DurabilityState::FileSaved,
            replacement_safety: ReplacementSafety::Safe,
            issues: Vec::new(),
        }
    }

    fn from_mutation(outcome: &MutationOutcome) -> Self {
        let issues = outcome
            .recovery_error
            .iter()
            .map(|diagnostic| PersistenceIssue::RecoveryWrite {
                diagnostic: diagnostic.clone(),
            })
            .collect();
        Self {
            durability: outcome.durability,
            replacement_safety: if outcome.durability == DurabilityState::Accepted {
                ReplacementSafety::MustRetainEditor
            } else {
                ReplacementSafety::Safe
            },
            issues,
        }
    }

    fn from_save(outcome: &SaveOutcome) -> Self {
        let mut issues = Vec::new();
        if let Some(diagnostic) = &outcome.canonical_error {
            let issue = if diagnostic.contains("durability is uncertain")
                || diagnostic.contains("parent sync failed")
            {
                PersistenceIssue::CanonicalDurabilityUncertain {
                    diagnostic: diagnostic.clone(),
                }
            } else {
                PersistenceIssue::CanonicalWrite {
                    diagnostic: diagnostic.clone(),
                }
            };
            issues.push(issue);
        }
        if let Some(diagnostic) = &outcome.recovery_cleanup_error {
            issues.push(PersistenceIssue::RecoveryCleanup {
                diagnostic: diagnostic.clone(),
            });
        }
        if let Some(diagnostic) = &outcome.index_error {
            issues.push(PersistenceIssue::IndexStale {
                diagnostic: diagnostic.clone(),
            });
        }
        if let Some(diagnostic) = &outcome.recency_error {
            issues.push(PersistenceIssue::RecencyUpdate {
                diagnostic: diagnostic.clone(),
            });
        }
        Self {
            durability: outcome.durability,
            replacement_safety: if outcome.durability == DurabilityState::Accepted {
                ReplacementSafety::MustRetainEditor
            } else {
                ReplacementSafety::Safe
            },
            issues,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictState {
    pub external_source: String,
    pub external_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingNativeDraftState {
    pub base_revision: u64,
    pub source: String,
    pub durable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecorationSet {
    pub revision: u64,
    pub items: Vec<Decoration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorPresentationState {
    pub snapshot: Snapshot,
    pub decorations: DecorationSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveEditorState {
    pub note_id: Identity,
    pub editor: EditorPresentationState,
    pub persistence: PersistenceState,
    pub conflict: Option<ConflictState>,
    pub pending_native_draft: Option<PendingNativeDraftState>,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderState {
    pub path: PathBuf,
    pub adopted: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationState {
    pub folder: Option<FolderState>,
    pub active: Option<ActiveEditorState>,
    pub recoveries: Vec<RecoveryDraft>,
    pub background_tasks: Vec<BackgroundTaskState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskState {
    pub id: String,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectResult {
    pub opened_note: Option<NoteSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteResult {
    pub note: NoteSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashResult {
    pub trash_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveResult {
    pub save: SaveOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationProblem {
    pub code: ProblemCode,
    pub diagnostic: String,
    pub details: Option<ProblemDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum OperationOutcome<T> {
    Applied(T),
    Rejected(ApplicationProblem),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationResponse<T> {
    pub state: ApplicationState,
    pub effects: Vec<HostEffect>,
    pub outcome: OperationOutcome<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostEffect {
    ScheduleAutosave {
        effect_id: String,
        delay_ms: u64,
        target: SaveTarget,
    },
    CancelEffect {
        effect_id: String,
    },
}

struct FolderContext {
    folder: NoteFolder,
    request: ConnectFolder,
}

struct ActiveEditor {
    note_id: Identity,
    editor: NoteEditor,
    persistence: PersistenceState,
    conflict: Option<ConflictState>,
    pending_native_draft: Option<PendingNativeDraftState>,
    autosave_effect_id: Option<String>,
    generation: u64,
}

pub struct ApplicationSession {
    services: Arc<ApplicationServices>,
    folder: Option<FolderContext>,
    active: Option<ActiveEditor>,
    recoveries: Vec<RecoveryDraft>,
    generation: u64,
    next_effect_id: u64,
}

impl Default for ApplicationSession {
    fn default() -> Self {
        Self::new(ApplicationServices::shared())
    }
}

impl ApplicationSession {
    pub fn new(services: Arc<ApplicationServices>) -> Self {
        Self {
            services,
            folder: None,
            active: None,
            recoveries: Vec::new(),
            generation: 0,
            next_effect_id: 0,
        }
    }

    pub fn services(&self) -> &Arc<ApplicationServices> {
        &self.services
    }

    pub fn state(&self) -> ApplicationState {
        ApplicationState {
            folder: self.folder.as_ref().map(|context| FolderState {
                path: context.folder.root().to_path_buf(),
                adopted: context.folder.is_adopted(),
                generation: self.generation,
            }),
            active: self.active.as_ref().map(|active| {
                let snapshot = active.editor.snapshot();
                let decorations = howler_editor::markdown_projection(&snapshot.source).0;
                let mut persistence = active.persistence.clone();
                if active.pending_native_draft.is_some() {
                    persistence.replacement_safety = ReplacementSafety::MustRetainEditor;
                }
                ActiveEditorState {
                    note_id: active.note_id.clone(),
                    editor: EditorPresentationState {
                        decorations: DecorationSet {
                            revision: snapshot.revision,
                            items: decorations,
                        },
                        snapshot,
                    },
                    persistence,
                    conflict: active.conflict.clone(),
                    pending_native_draft: active.pending_native_draft.clone(),
                    generation: active.generation,
                }
            }),
            recoveries: self.recoveries.clone(),
            background_tasks: Vec::new(),
        }
    }

    pub fn inspect(&self) -> ApplicationResponse<()> {
        self.applied((), Vec::new())
    }

    pub fn capabilities(&self) -> ApplicationResponse<SessionCapabilities> {
        self.applied(
            SessionCapabilities {
                application_session_abi: 2,
                selection_updates: true,
                input_origin_metadata: true,
                rust_owned_history: true,
                pending_native_drafts: true,
            },
            Vec::new(),
        )
    }

    pub fn connect(&mut self, request: ConnectFolder) -> ApplicationResponse<ConnectResult> {
        let mut effects = Vec::new();
        if let Err(problem) = self.prepare_replacement(&mut effects) {
            return self.rejected(problem, effects);
        }
        let opened = self.services.connect_folder(&request);
        let folder = match opened {
            Ok(folder) => folder,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        let recoveries = match folder.recoveries() {
            Ok(recoveries) => recoveries,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        self.cancel_active_effect(&mut effects);
        self.active = None;
        self.generation += 1;
        self.recoveries = recoveries;
        self.folder = Some(FolderContext {
            folder,
            request: request.clone(),
        });
        let notes = match self.folder_ref().and_then(NoteFolder::discover) {
            Ok(notes) => notes,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        let pending = match self
            .folder_ref()
            .and_then(NoteFolder::pending_native_drafts)
        {
            Ok(pending) => pending,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        let note = if notes.is_empty() {
            match self
                .folder_ref()
                .and_then(|folder| folder.create_note(None))
            {
                Ok(note) => Some(note),
                Err(error) => return self.rejected(problem_from_error(error), effects),
            }
        } else {
            pending
                .iter()
                .find_map(|draft| {
                    notes
                        .iter()
                        .find(|note| note.id.as_str() == draft.note_id)
                        .cloned()
                })
                .or_else(|| {
                    notes.into_iter().find(|note| {
                        !self
                            .recoveries
                            .iter()
                            .any(|draft| draft.note_id == note.id.as_str())
                    })
                })
        };
        let opened_note = if let Some(note) = note {
            if let Err(error) = self.install_editor(&note) {
                return self.rejected(problem_from_error(error), effects);
            }
            Some(note)
        } else {
            None
        };
        self.applied(ConnectResult { opened_note }, effects)
    }

    pub fn adopt_folder(&mut self) -> ApplicationResponse<ConnectResult> {
        let Some(context) = self.folder.as_ref() else {
            return self.rejected(
                simple_problem(ProblemCode::NotConnected, "no folder is connected"),
                Vec::new(),
            );
        };
        let mut request = context.request.clone();
        request.adopt = true;
        self.connect(request)
    }

    pub fn create_note(&mut self, request: CreateNote) -> ApplicationResponse<NoteResult> {
        let mut effects = Vec::new();
        if let Err(problem) = self.prepare_replacement(&mut effects) {
            return self.rejected(problem, effects);
        }
        let note = match self
            .folder_ref()
            .and_then(|folder| folder.create_note(request.source.as_deref()))
        {
            Ok(note) => note,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        self.cancel_active_effect(&mut effects);
        self.generation += 1;
        if let Err(error) = self.install_editor(&note) {
            return self.rejected(problem_from_error(error), effects);
        }
        self.refresh_recoveries();
        self.applied(NoteResult { note }, effects)
    }

    pub fn open_note(&mut self, id: &str) -> ApplicationResponse<NoteResult> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == id)
        {
            let note = match self.note_summary(id) {
                Ok(note) => note,
                Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
            };
            return self.applied(NoteResult { note }, Vec::new());
        }
        let mut effects = Vec::new();
        if let Err(problem) = self.prepare_replacement(&mut effects) {
            return self.rejected(problem, effects);
        }
        let note = match self.note_summary(id) {
            Ok(note) => note,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        self.cancel_active_effect(&mut effects);
        self.generation += 1;
        if let Err(error) = self.install_editor(&note) {
            return self.rejected(problem_from_error(error), effects);
        }
        self.applied(NoteResult { note }, effects)
    }

    pub fn close_note(&mut self) -> ApplicationResponse<()> {
        let mut effects = Vec::new();
        if let Err(problem) = self.prepare_replacement(&mut effects) {
            return self.rejected(problem, effects);
        }
        self.cancel_active_effect(&mut effects);
        self.active = None;
        self.generation += 1;
        self.applied((), effects)
    }

    pub fn apply_text_edit(&mut self, edit: HostTextEdit) -> ApplicationResponse<EditResult> {
        if self.active_has_pending_native_draft() {
            return self.rejected(
                simple_problem(
                    ProblemCode::InvalidOperation,
                    "pending native input must be resolved before editing",
                ),
                Vec::new(),
            );
        }
        let outcome = match self.active_mut() {
            Ok(active) => active.editor.apply(edit.into()),
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        self.finish_mutation(outcome)
    }

    pub fn apply_selection(
        &mut self,
        update: HostSelectionUpdate,
    ) -> ApplicationResponse<SelectionResult> {
        let (relative_path, previous_selections, selections) = match self.active_mut() {
            Ok(active) => {
                let relative_path = active.editor.relative_path.clone();
                let previous_selections = active.editor.snapshot().selections;
                match active
                    .editor
                    .set_selections(update.expected_revision, update.selections)
                {
                    Ok(selections) => (relative_path, previous_selections, selections),
                    Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                }
            }
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        if let Some(head) = selections.first().map(|selection| selection.head) {
            if let Err(error) = self
                .folder_ref()
                .and_then(|folder| folder.set_cursor(&relative_path, head))
            {
                let rollback = self.active_mut().and_then(|active| {
                    active
                        .editor
                        .set_selections(update.expected_revision, previous_selections)
                        .map_err(problem_from_error)
                });
                debug_assert!(
                    rollback.is_ok(),
                    "validated selection rollback must succeed"
                );
                return self.rejected(problem_from_error(error), Vec::new());
            }
        }
        self.applied(
            SelectionResult {
                revision: update.expected_revision,
                selections,
            },
            Vec::new(),
        )
    }

    pub fn execute_command(
        &mut self,
        expected_revision: u64,
        command: EditorCommand,
    ) -> ApplicationResponse<EditResult> {
        if self.active_has_pending_native_draft() {
            return self.rejected(
                simple_problem(
                    ProblemCode::InvalidOperation,
                    "pending native input must be resolved before editing",
                ),
                Vec::new(),
            );
        }
        let outcome = match self.active_mut() {
            Ok(active) => active.editor.execute_command(expected_revision, command),
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        self.finish_mutation(outcome)
    }

    pub fn undo(&mut self, expected_revision: u64) -> ApplicationResponse<Option<EditResult>> {
        if self.active_has_pending_native_draft() {
            return self.rejected(
                simple_problem(
                    ProblemCode::InvalidOperation,
                    "pending native input must be resolved before editing",
                ),
                Vec::new(),
            );
        }
        let outcome = match self.active_mut() {
            Ok(active) => active.editor.undo(expected_revision),
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        self.finish_optional_mutation(outcome)
    }

    pub fn redo(&mut self, expected_revision: u64) -> ApplicationResponse<Option<EditResult>> {
        if self.active_has_pending_native_draft() {
            return self.rejected(
                simple_problem(
                    ProblemCode::InvalidOperation,
                    "pending native input must be resolved before editing",
                ),
                Vec::new(),
            );
        }
        let outcome = match self.active_mut() {
            Ok(active) => active.editor.redo(expected_revision),
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        self.finish_optional_mutation(outcome)
    }

    pub fn preserve_pending_native_draft(
        &mut self,
        draft: PendingNativeDraft,
    ) -> ApplicationResponse<()> {
        let current_revision = match self.active.as_ref() {
            Some(active) => active.editor.snapshot().revision,
            None => {
                return self.rejected(
                    simple_problem(ProblemCode::InvalidOperation, "no note is open"),
                    Vec::new(),
                )
            }
        };
        if draft.base_revision > current_revision {
            return self.rejected(
                stale_problem(draft.base_revision, current_revision),
                Vec::new(),
            );
        }
        let mut effects = Vec::new();
        self.cancel_active_effect(&mut effects);
        let active = self.active.as_mut().unwrap();
        let persisted = active.editor.persist_pending_native_draft(&draft);
        let durable = persisted.is_ok();
        active.pending_native_draft = Some(PendingNativeDraftState {
            base_revision: draft.base_revision,
            source: draft.source,
            durable,
        });
        if let Err(error) = persisted {
            let issue = PersistenceIssue::RecoveryWrite {
                diagnostic: error.to_string(),
            };
            active.persistence.issues.push(issue.clone());
            active.persistence.replacement_safety = ReplacementSafety::MustRetainEditor;
            return self.rejected(
                ApplicationProblem {
                    code: ProblemCode::PersistenceFailure,
                    diagnostic: error.to_string(),
                    details: Some(ProblemDetails::Persistence {
                        issues: vec![issue],
                    }),
                },
                effects,
            );
        }
        active.persistence.replacement_safety = ReplacementSafety::MustRetainEditor;
        self.applied((), effects)
    }

    pub fn resolve_pending_native_draft(
        &mut self,
        resolution: PendingDraftResolution,
    ) -> ApplicationResponse<NoteResult> {
        match resolution {
            PendingDraftResolution::SaveAsNew {
                operation_id,
                title,
            } => {
                let request_key = serde_json::to_string(&("pending_save_as_new", &title)).unwrap();
                let prior = match self.folder_ref().and_then(|folder| {
                    folder.note_created_by_operation(&operation_id, &request_key)
                }) {
                    Ok(prior) => prior,
                    Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                };
                let pending = self
                    .active
                    .as_ref()
                    .and_then(|active| active.pending_native_draft.clone());
                let Some(pending) = pending else {
                    return match prior {
                        Some(note) => self.applied(NoteResult { note }, Vec::new()),
                        None => self.rejected(
                            simple_problem(
                                ProblemCode::InvalidOperation,
                                "no pending native draft",
                            ),
                            Vec::new(),
                        ),
                    };
                };
                let source = match title {
                    Some(title) => match rename_title_source(&pending.source, &title) {
                        Ok(source) => source,
                        Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                    },
                    None => pending.source,
                };
                let note = match self.folder_ref().and_then(|folder| {
                    folder.create_note_idempotent(&source, &operation_id, &request_key)
                }) {
                    Ok(note) => note,
                    Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                };
                if let Err(problem) = self.finish_pending_native_draft_resolution() {
                    return self.rejected(problem, Vec::new());
                }
                self.refresh_recoveries();
                self.applied(NoteResult { note }, Vec::new())
            }
            PendingDraftResolution::Discard => {
                if self
                    .active
                    .as_ref()
                    .and_then(|active| active.pending_native_draft.as_ref())
                    .is_none()
                {
                    return self.rejected(
                        simple_problem(ProblemCode::InvalidOperation, "no pending native draft"),
                        Vec::new(),
                    );
                }
                let note = match self.active.as_ref() {
                    Some(active) => match self.note_summary(active.note_id.as_str()) {
                        Ok(note) => note,
                        Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                    },
                    None => unreachable!(),
                };
                if let Err(problem) = self.finish_pending_native_draft_resolution() {
                    return self.rejected(problem, Vec::new());
                }
                self.refresh_recoveries();
                self.applied(NoteResult { note }, Vec::new())
            }
        }
    }

    pub fn save(&mut self, target: SaveTarget) -> ApplicationResponse<SaveResult> {
        let Some(active) = self.active.as_ref() else {
            return self.rejected(
                simple_problem(ProblemCode::InvalidOperation, "no note is open"),
                Vec::new(),
            );
        };
        let current_revision = active.editor.snapshot().revision;
        if active.note_id != target.note_id || active.generation != target.generation {
            return self.rejected(
                simple_problem(
                    ProblemCode::StaleEditor,
                    "save target no longer identifies the active editor",
                ),
                Vec::new(),
            );
        }
        if current_revision != target.revision {
            return self.rejected(stale_problem(target.revision, current_revision), Vec::new());
        }
        let result = self.save_current();
        match result {
            Ok(save) => self.applied(SaveResult { save }, Vec::new()),
            Err(problem) => self.rejected(problem, Vec::new()),
        }
    }

    pub fn restore_recovery(&mut self, id: &str) -> ApplicationResponse<NoteResult> {
        let mut effects = Vec::new();
        if let Err(problem) = self.prepare_replacement(&mut effects) {
            return self.rejected(problem, effects);
        }
        let editor = match self
            .folder_ref()
            .and_then(|folder| folder.restore_recovery(id))
        {
            Ok(editor) => editor,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        let note = match self.note_summary(id) {
            Ok(note) => note,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        self.cancel_active_effect(&mut effects);
        self.generation += 1;
        let pending_native_draft = match self
            .folder_ref()
            .and_then(NoteFolder::pending_native_drafts)
        {
            Ok(drafts) => drafts
                .into_iter()
                .find(|draft| draft.note_id == id)
                .map(|draft| PendingNativeDraftState {
                    base_revision: draft.base_revision,
                    source: draft.source,
                    durable: true,
                }),
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        self.active = Some(ActiveEditor {
            note_id: editor.note_id().clone(),
            editor,
            persistence: PersistenceState {
                durability: DurabilityState::RecoveryDurable,
                replacement_safety: if pending_native_draft.is_some() {
                    ReplacementSafety::MustRetainEditor
                } else {
                    ReplacementSafety::Safe
                },
                issues: Vec::new(),
            },
            conflict: None,
            pending_native_draft,
            autosave_effect_id: None,
            generation: self.generation,
        });
        self.refresh_recoveries();
        self.applied(NoteResult { note }, effects)
    }

    pub fn discard_recovery(&mut self, id: &str) -> ApplicationResponse<()> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == id && active.editor.is_dirty())
        {
            return self.rejected(
                simple_problem(
                    ProblemCode::InvalidOperation,
                    "cannot discard recovery for a dirty active editor",
                ),
                Vec::new(),
            );
        }
        match self
            .folder_ref()
            .and_then(|folder| folder.discard_recovery(id))
        {
            Ok(()) => {
                self.refresh_recoveries();
                self.applied((), Vec::new())
            }
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn reconcile_active(&mut self) -> ApplicationResponse<ReconcileResult> {
        let result = match self.active_mut() {
            Ok(active) => active.editor.reconcile_external(),
            Err(problem) => return self.rejected(problem, Vec::new()),
        };
        match result {
            Ok(result) => {
                if let ReconcileResult::Conflict { external_source } = &result {
                    if let Some(active) = &mut self.active {
                        active.conflict = Some(ConflictState {
                            external_hash: hash(external_source),
                            external_source: external_source.clone(),
                        });
                    }
                } else if let Some(active) = &mut self.active {
                    active.conflict = None;
                }
                self.applied(result, Vec::new())
            }
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn resolve_conflict(
        &mut self,
        resolution: ConflictResolution,
    ) -> ApplicationResponse<NoteResult> {
        let prior = match &resolution {
            ConflictResolution::UseExternal { .. } => None,
            ConflictResolution::KeepLocalAsNewNote {
                operation_id,
                expected_external_hash,
                title,
            } => {
                let request_key =
                    serde_json::to_string(&("conflict_keep_local", expected_external_hash, title))
                        .unwrap();
                match self
                    .folder_ref()
                    .and_then(|folder| folder.note_created_by_operation(operation_id, &request_key))
                {
                    Ok(note) => note,
                    Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                }
            }
        };
        let conflict = self
            .active
            .as_ref()
            .and_then(|active| active.conflict.clone());
        let Some(conflict) = conflict else {
            return match prior {
                Some(note) => self.applied(NoteResult { note }, Vec::new()),
                None => self.rejected(
                    simple_problem(ProblemCode::InvalidOperation, "no external conflict"),
                    Vec::new(),
                ),
            };
        };
        let (expected_hash, operation_id, title, request_key) = match resolution {
            ConflictResolution::UseExternal {
                expected_external_hash,
            } => (expected_external_hash, None, None, None),
            ConflictResolution::KeepLocalAsNewNote {
                operation_id,
                expected_external_hash,
                title,
            } => {
                let request_key = serde_json::to_string(&(
                    "conflict_keep_local",
                    &expected_external_hash,
                    &title,
                ))
                .unwrap();
                (
                    expected_external_hash,
                    Some(operation_id),
                    title,
                    Some(request_key),
                )
            }
        };
        if expected_hash != conflict.external_hash {
            return self.rejected(
                ApplicationProblem {
                    code: ProblemCode::ContentHashMismatch,
                    diagnostic: "external content changed before conflict resolution".into(),
                    details: Some(ProblemDetails::ContentHashMismatch {
                        expected_hash,
                        current_hash: conflict.external_hash,
                    }),
                },
                Vec::new(),
            );
        }
        let mut created_note = None;
        if let Some(operation_id) = operation_id {
            let local = self.active.as_ref().unwrap().editor.snapshot().source;
            let local = match title {
                Some(title) => match rename_title_source(&local, &title) {
                    Ok(source) => source,
                    Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
                },
                None => local,
            };
            match self.folder_ref().and_then(|folder| {
                folder.create_note_idempotent(&local, &operation_id, request_key.as_ref().unwrap())
            }) {
                Ok(note) => created_note = Some(note),
                Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
            }
        }
        let install = self
            .active
            .as_mut()
            .unwrap()
            .editor
            .install_external(&expected_hash);
        if let Err(error) = install {
            let problem = problem_from_error(error);
            if let Some(ProblemDetails::ExternalConflict {
                external_source,
                external_hash,
            }) = &problem.details
            {
                self.active.as_mut().unwrap().conflict = Some(ConflictState {
                    external_source: external_source.clone(),
                    external_hash: external_hash.clone(),
                });
            }
            return self.rejected(problem, Vec::new());
        }
        let id = self.active.as_ref().unwrap().note_id.as_str().to_owned();
        let note = match self.note_summary(&id) {
            Ok(note) => note,
            Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
        };
        let active = self.active.as_mut().unwrap();
        active.conflict = None;
        active.persistence = PersistenceState::saved();
        self.refresh_recoveries();
        self.applied(
            NoteResult {
                note: created_note.unwrap_or(note),
            },
            Vec::new(),
        )
    }

    pub fn search(&self, query: SearchQuery) -> ApplicationResponse<Vec<SearchResult>> {
        match self
            .folder_ref()
            .and_then(|folder| folder.search(&query.query, query.limit))
        {
            Ok(results) => self.applied(results, Vec::new()),
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn rename_note(&mut self, request: RenameNote) -> ApplicationResponse<NoteResult> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == request.note_id)
        {
            let mut effects = Vec::new();
            if let Err(problem) = self.prepare_replacement(&mut effects) {
                return self.rejected(problem, effects);
            }
            let result = {
                let folder = &self.folder.as_ref().unwrap().folder;
                let active = self.active.as_mut().unwrap();
                folder.rename_editor_title(&mut active.editor, &request.title)
            };
            return match result {
                Ok((note, save)) => {
                    self.active.as_mut().unwrap().persistence = PersistenceState::from_save(&save);
                    self.refresh_recoveries();
                    self.applied(NoteResult { note }, effects)
                }
                Err(error) => self.rejected(problem_from_error(error), effects),
            };
        }
        self.lifecycle_note_mutation(&request.note_id, |folder| {
            folder.rename_title(&request.note_id, &request.title)
        })
    }

    pub fn move_note(&mut self, request: MoveNote) -> ApplicationResponse<NoteResult> {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == request.note_id)
        {
            let mut effects = Vec::new();
            if let Err(problem) = self.prepare_replacement(&mut effects) {
                return self.rejected(problem, effects);
            }
            let result = {
                let folder = &self.folder.as_ref().unwrap().folder;
                let active = self.active.as_mut().unwrap();
                folder.move_editor(&mut active.editor, &request.destination)
            };
            return match result {
                Ok(note) => {
                    self.active.as_mut().unwrap().note_id = note.id.clone();
                    self.applied(NoteResult { note }, effects)
                }
                Err(error) => self.rejected(problem_from_error(error), effects),
            };
        }
        self.lifecycle_note_mutation(&request.note_id, |folder| {
            folder.move_note(&request.note_id, &request.destination)
        })
    }

    pub fn trash_note(&mut self, id: &str) -> ApplicationResponse<TrashResult> {
        let active_matches = self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == id);
        let mut effects = Vec::new();
        if active_matches {
            if let Err(problem) = self.prepare_replacement(&mut effects) {
                return self.rejected(problem, effects);
            }
        }
        match self.folder_ref().and_then(|folder| folder.trash(id)) {
            Ok(trash_path) => {
                if active_matches {
                    self.cancel_active_effect(&mut effects);
                    self.active = None;
                    self.generation += 1;
                }
                self.applied(TrashResult { trash_path }, effects)
            }
            Err(error) => self.rejected(problem_from_error(error), effects),
        }
    }

    pub fn restore_note(&mut self, request: RestoreNote) -> ApplicationResponse<NoteResult> {
        match self
            .folder_ref()
            .and_then(|folder| folder.restore(&request.trash_path))
        {
            Ok(note) => self.applied(NoteResult { note }, Vec::new()),
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn rescan_synchronous(&mut self) -> ApplicationResponse<RescanReport> {
        let report = match self.folder_ref().and_then(NoteFolder::rescan) {
            Ok(report) => report,
            Err(error) => return self.rejected(problem_from_error(error), Vec::new()),
        };
        if self.active.is_some() {
            let reconciliation = self.reconcile_active();
            if let OperationOutcome::Rejected(problem) = reconciliation.outcome {
                return self.rejected(problem, reconciliation.effects);
            }
        }
        self.applied(report, Vec::new())
    }

    pub fn rebuild_synchronous(&self) -> ApplicationResponse<RebuildReport> {
        match self.folder_ref().and_then(NoteFolder::rebuild_index) {
            Ok(report) => self.applied(report, Vec::new()),
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn diagnostics(&self) -> ApplicationResponse<Vec<Diagnostic>> {
        match self.folder_ref().and_then(NoteFolder::diagnostics) {
            Ok(diagnostics) => self.applied(diagnostics, Vec::new()),
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    pub fn diagnostic_bundle(&self) -> ApplicationResponse<DiagnosticBundle> {
        match self.folder_ref().and_then(NoteFolder::diagnostic_bundle) {
            Ok(bundle) => self.applied(bundle, Vec::new()),
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    fn lifecycle_note_mutation(
        &mut self,
        id: &str,
        operation: impl FnOnce(&NoteFolder) -> Result<NoteSummary, AppError>,
    ) -> ApplicationResponse<NoteResult> {
        let active_matches = self
            .active
            .as_ref()
            .is_some_and(|active| active.note_id.as_str() == id);
        let mut effects = Vec::new();
        if active_matches {
            if let Err(problem) = self.prepare_replacement(&mut effects) {
                return self.rejected(problem, effects);
            }
        }
        let note = match self.folder_ref().and_then(operation) {
            Ok(note) => note,
            Err(error) => return self.rejected(problem_from_error(error), effects),
        };
        if active_matches {
            self.cancel_active_effect(&mut effects);
            self.generation += 1;
            if let Err(error) = self.install_editor(&note) {
                return self.rejected(problem_from_error(error), effects);
            }
        }
        self.applied(NoteResult { note }, effects)
    }

    fn finish_mutation(
        &mut self,
        outcome: Result<MutationOutcome, AppError>,
    ) -> ApplicationResponse<EditResult> {
        match outcome {
            Ok(outcome) => {
                let Some(edit) = outcome.edit.clone() else {
                    return self.rejected(
                        simple_problem(ProblemCode::InvalidOperation, "mutation produced no edit"),
                        Vec::new(),
                    );
                };
                self.active.as_mut().unwrap().persistence =
                    PersistenceState::from_mutation(&outcome);
                self.active.as_mut().unwrap().conflict = None;
                self.refresh_recoveries();
                let effects = self.schedule_autosave();
                self.applied(edit, effects)
            }
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    fn finish_optional_mutation(
        &mut self,
        outcome: Result<MutationOutcome, AppError>,
    ) -> ApplicationResponse<Option<EditResult>> {
        match outcome {
            Ok(outcome) => {
                self.active.as_mut().unwrap().persistence =
                    PersistenceState::from_mutation(&outcome);
                self.refresh_recoveries();
                let effects = if outcome.edit.is_some() {
                    self.schedule_autosave()
                } else {
                    Vec::new()
                };
                self.applied(outcome.edit, effects)
            }
            Err(error) => self.rejected(problem_from_error(error), Vec::new()),
        }
    }

    fn prepare_replacement(
        &mut self,
        effects: &mut Vec<HostEffect>,
    ) -> Result<(), ApplicationProblem> {
        if self.active.is_none() {
            return Ok(());
        }
        if self.active.as_ref().unwrap().pending_native_draft.is_some() {
            return Err(self.persistence_problem(
                "pending native input must be resolved before replacing the editor",
            ));
        }
        if self.active.as_ref().unwrap().persistence.replacement_safety
            == ReplacementSafety::MustRetainEditor
        {
            return Err(self.persistence_problem("active editor has no durable copy"));
        }
        if self.active.as_ref().unwrap().editor.is_dirty() {
            match self.save_current() {
                Ok(_) => {}
                Err(problem) => {
                    if self.active.as_ref().unwrap().persistence.replacement_safety
                        == ReplacementSafety::MustRetainEditor
                        || matches!(
                            problem.code,
                            ProblemCode::ExternalConflict
                                | ProblemCode::IdentityChanged
                                | ProblemCode::StaleEditor
                        )
                    {
                        return Err(problem);
                    }
                }
            }
        }
        self.cancel_active_effect(effects);
        Ok(())
    }

    fn persistence_problem(&self, diagnostic: &str) -> ApplicationProblem {
        ApplicationProblem {
            code: ProblemCode::PersistenceFailure,
            diagnostic: diagnostic.into(),
            details: Some(ProblemDetails::Persistence {
                issues: self
                    .active
                    .as_ref()
                    .map(|active| active.persistence.issues.clone())
                    .unwrap_or_default(),
            }),
        }
    }

    fn finish_pending_native_draft_resolution(&mut self) -> Result<(), ApplicationProblem> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| simple_problem(ProblemCode::InvalidOperation, "no note is open"))?;
        if active.editor.is_dirty() {
            if let Err(error) = active.editor.persist_active_recovery() {
                let issue = PersistenceIssue::RecoveryWrite {
                    diagnostic: error.to_string(),
                };
                active.persistence.issues.push(issue.clone());
                active.persistence.replacement_safety = ReplacementSafety::MustRetainEditor;
                return Err(ApplicationProblem {
                    code: ProblemCode::PersistenceFailure,
                    diagnostic: error.to_string(),
                    details: Some(ProblemDetails::Persistence {
                        issues: vec![issue],
                    }),
                });
            }
            active.persistence.durability = DurabilityState::RecoveryDurable;
        }
        if let Err(error) = active.editor.clear_pending_native_draft() {
            let issue = PersistenceIssue::RecoveryCleanup {
                diagnostic: error.to_string(),
            };
            active.persistence.issues.push(issue.clone());
            active.persistence.replacement_safety = ReplacementSafety::MustRetainEditor;
            return Err(ApplicationProblem {
                code: ProblemCode::PersistenceFailure,
                diagnostic: error.to_string(),
                details: Some(ProblemDetails::Persistence {
                    issues: vec![issue],
                }),
            });
        }
        active.pending_native_draft = None;
        active.persistence.replacement_safety = ReplacementSafety::Safe;
        Ok(())
    }

    fn save_current(&mut self) -> Result<SaveOutcome, ApplicationProblem> {
        let revision = self
            .active
            .as_ref()
            .ok_or_else(|| simple_problem(ProblemCode::InvalidOperation, "no note is open"))?
            .editor
            .snapshot()
            .revision;
        let result = {
            let context = self.folder.as_ref().ok_or_else(|| {
                simple_problem(ProblemCode::NotConnected, "no folder is connected")
            })?;
            let active = self.active.as_mut().unwrap();
            context.folder.save_editor(&mut active.editor, revision)
        };
        match result {
            Ok(save) => {
                self.active.as_mut().unwrap().persistence = PersistenceState::from_save(&save);
                if self.active_has_pending_native_draft() {
                    self.active.as_mut().unwrap().persistence.replacement_safety =
                        ReplacementSafety::MustRetainEditor;
                }
                self.active.as_mut().unwrap().conflict = None;
                self.refresh_recoveries();
                Ok(save)
            }
            Err(AppError::ExternalConflict { external_source }) => {
                let conflict = ConflictState {
                    external_hash: hash(&external_source),
                    external_source: external_source.clone(),
                };
                self.active.as_mut().unwrap().conflict = Some(conflict);
                self.refresh_recoveries();
                Err(problem_from_error(AppError::ExternalConflict {
                    external_source,
                }))
            }
            Err(error) => {
                self.refresh_recoveries();
                if matches!(error, AppError::Io(_) | AppError::Database(_)) {
                    let issue = PersistenceIssue::CanonicalWrite {
                        diagnostic: error.to_string(),
                    };
                    let active = self.active.as_mut().unwrap();
                    active.persistence.issues.push(issue.clone());
                    active.persistence.replacement_safety =
                        if active.persistence.durability == DurabilityState::Accepted {
                            ReplacementSafety::MustRetainEditor
                        } else {
                            ReplacementSafety::Safe
                        };
                    Err(ApplicationProblem {
                        code: ProblemCode::PersistenceFailure,
                        diagnostic: error.to_string(),
                        details: Some(ProblemDetails::Persistence {
                            issues: vec![issue],
                        }),
                    })
                } else {
                    Err(problem_from_error(error))
                }
            }
        }
    }

    fn schedule_autosave(&mut self) -> Vec<HostEffect> {
        let mut effects = Vec::new();
        self.cancel_active_effect(&mut effects);
        self.next_effect_id += 1;
        let effect_id = format!("autosave-{}", self.next_effect_id);
        let active = self.active.as_mut().unwrap();
        active.autosave_effect_id = Some(effect_id.clone());
        effects.push(HostEffect::ScheduleAutosave {
            effect_id,
            delay_ms: 750,
            target: SaveTarget {
                note_id: active.note_id.clone(),
                revision: active.editor.snapshot().revision,
                generation: active.generation,
            },
        });
        effects
    }

    fn cancel_active_effect(&mut self, effects: &mut Vec<HostEffect>) {
        if let Some(effect_id) = self
            .active
            .as_mut()
            .and_then(|active| active.autosave_effect_id.take())
        {
            effects.push(HostEffect::CancelEffect { effect_id });
        }
    }

    fn install_editor(&mut self, note: &NoteSummary) -> Result<(), AppError> {
        let pending = self
            .folder_ref()?
            .pending_native_drafts()?
            .into_iter()
            .find(|draft| draft.note_id == note.id.as_str());
        let recovery_pending = self
            .recoveries
            .iter()
            .any(|draft| draft.note_id == note.id.as_str());
        let editor = if pending.is_some() && recovery_pending {
            self.folder_ref()?
                .restore_recovery_with_pending(note.id.as_str(), true)?
        } else {
            self.folder_ref()?
                .open_editor_with_pending(note.id.as_str(), pending.is_some())?
        };
        let pending_native_draft = pending.map(|draft| PendingNativeDraftState {
            base_revision: draft.base_revision,
            source: draft.source,
            durable: true,
        });
        self.active = Some(ActiveEditor {
            note_id: note.id.clone(),
            editor,
            persistence: if pending_native_draft.is_some() {
                PersistenceState {
                    durability: if recovery_pending {
                        DurabilityState::RecoveryDurable
                    } else {
                        DurabilityState::FileSaved
                    },
                    replacement_safety: ReplacementSafety::MustRetainEditor,
                    issues: Vec::new(),
                }
            } else {
                PersistenceState::saved()
            },
            conflict: None,
            pending_native_draft,
            autosave_effect_id: None,
            generation: self.generation,
        });
        Ok(())
    }

    fn note_summary(&self, id: &str) -> Result<NoteSummary, AppError> {
        self.folder_ref()?
            .discover()?
            .into_iter()
            .find(|note| note.id.as_str() == id)
            .ok_or_else(|| AppError::NoteNotFound(id.into()))
    }

    fn folder_ref(&self) -> Result<&NoteFolder, AppError> {
        self.folder
            .as_ref()
            .map(|context| &context.folder)
            .ok_or_else(|| {
                AppError::Io(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "no folder is connected",
                ))
            })
    }

    fn active_mut(&mut self) -> Result<&mut ActiveEditor, ApplicationProblem> {
        self.active
            .as_mut()
            .ok_or_else(|| simple_problem(ProblemCode::InvalidOperation, "no note is open"))
    }

    fn active_has_pending_native_draft(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.pending_native_draft.is_some())
    }

    fn refresh_recoveries(&mut self) {
        if let Some(folder) = &self.folder {
            if let Ok(recoveries) = folder.folder.recoveries() {
                self.recoveries = recoveries;
            }
        }
    }

    fn applied<T>(&self, value: T, effects: Vec<HostEffect>) -> ApplicationResponse<T> {
        ApplicationResponse {
            state: self.state(),
            effects,
            outcome: OperationOutcome::Applied(value),
        }
    }

    fn rejected<T>(
        &self,
        problem: ApplicationProblem,
        effects: Vec<HostEffect>,
    ) -> ApplicationResponse<T> {
        ApplicationResponse {
            state: self.state(),
            effects,
            outcome: OperationOutcome::Rejected(problem),
        }
    }
}

fn simple_problem(code: ProblemCode, diagnostic: impl Into<String>) -> ApplicationProblem {
    ApplicationProblem {
        code,
        diagnostic: diagnostic.into(),
        details: None,
    }
}

fn stale_problem(expected_revision: u64, current_revision: u64) -> ApplicationProblem {
    ApplicationProblem {
        code: ProblemCode::StaleRevision,
        diagnostic: format!(
            "expected revision {expected_revision}, current revision is {current_revision}"
        ),
        details: Some(ProblemDetails::StaleRevision {
            expected_revision,
            current_revision,
        }),
    }
}

fn problem_from_error(error: AppError) -> ApplicationProblem {
    let diagnostic = error.to_string();
    let (code, details) = match error {
        AppError::NoteNotFound(_) => (ProblemCode::NoteNotFound, None),
        AppError::RecoveryNotFound(_) => (ProblemCode::RecoveryNotFound, None),
        AppError::RecoveryPending(id) => {
            let identity = if id.len() == 26 {
                Identity::Adopted(id)
            } else {
                Identity::Provisional(id)
            };
            (
                ProblemCode::RecoveryPending,
                Some(ProblemDetails::RecoveryPending { note_id: identity }),
            )
        }
        AppError::Editor(EditorError::StaleRevision { expected, actual }) => (
            ProblemCode::StaleRevision,
            Some(ProblemDetails::StaleRevision {
                expected_revision: expected,
                current_revision: actual,
            }),
        ),
        AppError::ExternalConflict { external_source } => {
            let external_hash = hash(&external_source);
            (
                ProblemCode::ExternalConflict,
                Some(ProblemDetails::ExternalConflict {
                    external_source,
                    external_hash,
                }),
            )
        }
        AppError::IdentityChanged => (ProblemCode::IdentityChanged, None),
        AppError::StaleHandle => (ProblemCode::StaleEditor, None),
        AppError::NoteAlreadyOpen(_) | AppError::PendingNativeDraft(_) => {
            (ProblemCode::InvalidOperation, None)
        }
        AppError::WrongOwner => (ProblemCode::WrongOwner, None),
        AppError::DestinationExists(_) => (ProblemCode::DestinationExists, None),
        AppError::DuplicateIdentity(_) => (ProblemCode::DuplicateIdentity, None),
        AppError::Database(_) => (ProblemCode::DatabaseFailure, None),
        AppError::Io(ref io_error) if io_error.kind() == io::ErrorKind::NotConnected => {
            (ProblemCode::NotConnected, None)
        }
        AppError::Io(_)
        | AppError::InvalidUtf8(_)
        | AppError::Editor(_)
        | AppError::PathEscape
        | AppError::MalformedMetadata(_)
        | AppError::InvalidTitle => (ProblemCode::InvalidOperation, None),
    };
    ApplicationProblem {
        code,
        diagnostic,
        details,
    }
}

#[derive(Debug)]
struct IndexValue {
    id: String,
    adopted: bool,
    path: String,
    title: String,
    body: String,
    hash: String,
    modified: i64,
}

fn initialize_index(index: &Connection) -> Result<(), rusqlite::Error> {
    index.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS migrations(version INTEGER PRIMARY KEY);
         INSERT OR IGNORE INTO migrations VALUES(2);
         CREATE TABLE IF NOT EXISTS notes(id TEXT NOT NULL, adopted INTEGER NOT NULL, path TEXT NOT NULL UNIQUE, title TEXT NOT NULL, body TEXT NOT NULL, hash TEXT NOT NULL, modified INTEGER NOT NULL);
         CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5(title, body);",
    )
}

fn initialize_state(state: &Connection) -> Result<(), rusqlite::Error> {
    state.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS migrations(version INTEGER PRIMARY KEY);
         INSERT OR IGNORE INTO migrations VALUES(1);
         CREATE TABLE IF NOT EXISTS stale_index(path TEXT PRIMARY KEY, error TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS recent_notes(path TEXT PRIMARY KEY, last_opened INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS cursor_state(path TEXT PRIMARY KEY, byte_offset INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS sequence(name TEXT PRIMARY KEY, value INTEGER NOT NULL);",
    )
}

fn index_values(
    id: &Identity,
    relative: &Path,
    source: &str,
    content_hash: &str,
    absolute: &Path,
) -> Result<IndexValue, AppError> {
    let modified = fs::metadata(absolute)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    Ok(IndexValue {
        id: id.as_str().into(),
        adopted: matches!(id, Identity::Adopted(_)),
        path: relative.to_string_lossy().into(),
        title: derive_title(source),
        body: howler_editor::markdown_projection(source).1,
        hash: content_hash.into(),
        modified,
    })
}

fn insert_index_value(
    transaction: &SqlTransaction<'_>,
    value: &IndexValue,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO notes(id,adopted,path,title,body,hash,modified) VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(path) DO UPDATE SET id=excluded.id,adopted=excluded.adopted,title=excluded.title,body=excluded.body,hash=excluded.hash,modified=excluded.modified",
        params![value.id, value.adopted, value.path, value.title, value.body, value.hash, value.modified],
    )?;
    let rowid: i64 = transaction.query_row(
        "SELECT rowid FROM notes WHERE path=?1",
        params![value.path],
        |row| row.get(0),
    )?;
    transaction.execute("DELETE FROM note_fts WHERE rowid=?1", params![rowid])?;
    transaction.execute(
        "INSERT INTO note_fts(rowid,title,body) VALUES(?1,?2,?3)",
        params![rowid, value.title, value.body],
    )?;
    Ok(())
}

fn discover_notes(root: &Path) -> Result<Vec<NoteSummary>, AppError> {
    let mut notes = Vec::new();
    for entry in markdown_entries(root) {
        let entry = entry.map_err(walkdir_error)?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| AppError::PathEscape)?
            .to_path_buf();
        let source = read_utf8(entry.path(), &relative)?;
        let content_hash = hash(&source);
        let identity = identity_for_source(&relative, &source)?;
        notes.push(NoteSummary {
            id: identity,
            relative_path: relative,
            title: derive_title(&source),
            content_hash,
        });
    }
    notes.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(notes)
}

fn markdown_entries(root: &Path) -> impl Iterator<Item = Result<DirEntry, walkdir::Error>> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !(entry.file_type().is_dir() && (name == ".howler" || name == ".trash"))
                && !entry.file_type().is_symlink()
        })
        .filter(|entry| match entry {
            Ok(entry) => entry.file_type().is_file() && is_markdown(entry.path()),
            Err(_) => true,
        })
}

fn walkdir_error(error: walkdir::Error) -> AppError {
    let kind = error
        .io_error()
        .map(io::Error::kind)
        .unwrap_or(io::ErrorKind::Other);
    AppError::Io(io::Error::new(kind, "directory walk failed"))
}

fn validate_no_symlink_components(
    root: &Path,
    relative: &Path,
    leaf_may_be_missing: bool,
) -> Result<PathBuf, AppError> {
    let relative = normalized_relative(relative)?;
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(component) = component else {
            return Err(AppError::PathEscape);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(AppError::PathEscape),
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && (leaf_may_be_missing || index + 1 < count) =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root.join(relative))
}

fn validate_identity(id: &str) -> Result<(), AppError> {
    let valid_ulid = validate_adopted_identity(id).is_ok();
    let valid_provisional = id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid_ulid || valid_provisional {
        Ok(())
    } else {
        Err(AppError::MalformedMetadata("invalid howler_id".into()))
    }
}

fn validate_operation_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        Err(AppError::MalformedMetadata(
            "operation_id must be 1-128 ASCII letters, digits, '.', '-', or '_'".into(),
        ))
    } else {
        Ok(())
    }
}

fn validate_adopted_identity(id: &str) -> Result<(), AppError> {
    if id.len() == 26 && Ulid::from_string(id).is_ok() {
        Ok(())
    } else {
        Err(AppError::MalformedMetadata("invalid howler_id".into()))
    }
}

fn identity_for_source(relative: &Path, source: &str) -> Result<Identity, AppError> {
    match metadata(source).id {
        Some(id) => {
            validate_adopted_identity(&id)?;
            Ok(Identity::Adopted(id))
        }
        None => Ok(Identity::Provisional(provisional_id(relative))),
    }
}

fn ensure_same_identity(expected: &Identity, source: &str) -> Result<(), AppError> {
    let actual = metadata(source).id;
    let matches = match (expected, actual.as_deref()) {
        (Identity::Adopted(expected), Some(actual)) => {
            validate_adopted_identity(actual)?;
            expected == actual
        }
        (Identity::Provisional(_), None) => true,
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(AppError::IdentityChanged)
    }
}

fn normalized_relative(path: &Path) -> Result<PathBuf, AppError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AppError::PathEscape);
    }
    Ok(path.to_path_buf())
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn read_utf8(path: &Path, display: &Path) -> Result<String, AppError> {
    String::from_utf8(fs::read(path)?)
        .map_err(|_| AppError::InvalidUtf8(display.to_string_lossy().into()))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().ok_or(AppError::PathEscape)?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_parent(parent)
}

fn create_no_replace(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().ok_or(AppError::PathEscape)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file_mut().sync_all()?;
    match fs::hard_link(temporary.path(), path) {
        Ok(()) => sync_parent(parent),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(
            AppError::DestinationExists(path.file_name().unwrap_or_default().into()),
        ),
        Err(error) => Err(error.into()),
    }
}

fn move_no_replace(source: &Path, destination: &Path) -> Result<(), io::Error> {
    fs::hard_link(source, destination)?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    if let Err(error) = sync_directory(destination_parent) {
        return match fs::remove_file(destination).and_then(|()| sync_directory(destination_parent))
        {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::new(
                cleanup.kind(),
                format!("directory sync failed: {error}; rollback failed: {cleanup}"),
            )),
        };
    }
    if let Err(error) = fs::remove_file(source) {
        return match fs::remove_file(destination).and_then(|()| sync_directory(destination_parent))
        {
            Ok(()) => Err(error),
            Err(cleanup) => Err(io::Error::new(
                cleanup.kind(),
                format!("source removal failed: {error}; rollback failed: {cleanup}"),
            )),
        };
    }
    let source_parent = source
        .parent()
        .ok_or_else(|| io::Error::other("source has no parent"))?;
    sync_directory(source_parent)
}

fn sync_parent(parent: &Path) -> Result<(), AppError> {
    sync_directory(parent).map_err(AppError::Io)
}

fn sync_directory(parent: &Path) -> Result<(), io::Error> {
    File::open(parent)?.sync_all()
}

fn hash(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

fn provisional_folder_id(root: &Path) -> String {
    blake3::hash(root.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

fn provisional_id(relative: &Path) -> String {
    blake3::hash(relative.to_string_lossy().as_bytes())
        .to_hex()
        .to_string()
}

fn safe_key(id: &str) -> String {
    id.chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect()
}

fn fuzzy_subsequence(value: &str, query: &str) -> bool {
    let mut value = value.chars();
    query
        .chars()
        .all(|expected| value.by_ref().any(|actual| actual == expected))
}

struct Metadata {
    id: Option<String>,
    title: Option<String>,
    error: Option<String>,
}

fn metadata(source: &str) -> Metadata {
    let Some(end) = front_matter_end(source) else {
        return Metadata {
            id: None,
            title: None,
            error: None,
        };
    };
    let first_line = source.find('\n').unwrap_or(3) + 1;
    let closing_len = if source[..end].ends_with("---\r\n") {
        5
    } else {
        4
    };
    let yaml = &source[first_line..end - closing_len];
    match serde_yaml::from_str::<Value>(yaml) {
        Ok(Value::Mapping(values)) => Metadata {
            id: values
                .get(Value::String("howler_id".into()))
                .and_then(Value::as_str)
                .map(str::to_owned),
            title: values
                .get(Value::String("title".into()))
                .and_then(Value::as_str)
                .map(str::to_owned),
            error: None,
        },
        Ok(_) => Metadata {
            id: None,
            title: None,
            error: Some("front matter must be a mapping".into()),
        },
        Err(error) => Metadata {
            id: None,
            title: None,
            error: Some(error.to_string()),
        },
    }
}

fn derive_title(source: &str) -> String {
    let parsed = metadata(source);
    if let Some(title) = parsed.title.filter(|title| !title.trim().is_empty()) {
        return title;
    }
    let body = &source[front_matter_end(source).unwrap_or(0)..];
    if let Some(heading) = body.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) {
        return heading.into();
    }
    howler_editor::markdown_projection(source)
        .1
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("Untitled")
        .into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdoptionMapping {
    relative_path: PathBuf,
    provisional_id: String,
    adopted_id: String,
    pre_adoption_hash: String,
    adopted_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdoptionManifest {
    library_id: String,
    mappings: Vec<AdoptionMapping>,
}

fn load_or_create_adoption_manifest(
    root: &Path,
    provisional: &Path,
    library_id: Option<&str>,
) -> Result<AdoptionManifest, AppError> {
    fs::create_dir_all(provisional)?;
    let path = provisional.join("adoption.json");
    if path.exists() {
        let manifest: AdoptionManifest = serde_json::from_slice(&fs::read(path)?)
            .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
        validate_adopted_identity(&manifest.library_id)?;
        if library_id.is_some_and(|expected| expected != manifest.library_id) {
            return Err(AppError::IdentityChanged);
        }
        return Ok(manifest);
    }
    let manifest = AdoptionManifest {
        library_id: library_id
            .map(str::to_owned)
            .unwrap_or_else(|| Ulid::new().to_string()),
        mappings: adoption_mappings(root)?,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    match create_no_replace(&path, &bytes) {
        Ok(()) => Ok(manifest),
        Err(AppError::DestinationExists(_)) => {
            let persisted: AdoptionManifest = serde_json::from_slice(&fs::read(path)?)
                .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
            validate_adopted_identity(&persisted.library_id)?;
            if library_id.is_some_and(|expected| expected != persisted.library_id) {
                return Err(AppError::IdentityChanged);
            }
            Ok(persisted)
        }
        Err(error) => Err(error),
    }
}

fn adoption_mappings(root: &Path) -> Result<Vec<AdoptionMapping>, AppError> {
    let mut mappings = Vec::new();
    for entry in markdown_entries(root) {
        let entry = entry.map_err(walkdir_error)?;
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| AppError::PathEscape)?;
        let source = read_utf8(entry.path(), relative)?;
        if let Some(id) = metadata(&source).id {
            validate_adopted_identity(&id)?;
        } else {
            let id = Ulid::new().to_string();
            let adopted = adopt_source(&source, &id);
            mappings.push(AdoptionMapping {
                relative_path: relative.to_path_buf(),
                provisional_id: provisional_id(relative),
                adopted_id: id,
                pre_adoption_hash: hash(&source),
                adopted_hash: hash(&adopted),
            });
        }
    }
    Ok(mappings)
}

fn apply_adoption_manifest(
    root: &Path,
    manifest: &AdoptionManifest,
    generations: &Arc<Mutex<HashMap<String, u64>>>,
) -> Result<(), AppError> {
    for mapping in &manifest.mappings {
        let path = validate_no_symlink_components(root, &mapping.relative_path, false)?;
        let source = read_utf8(&path, &mapping.relative_path)?;
        match metadata(&source).id {
            Some(id) if id == mapping.adopted_id => continue,
            Some(_) => return Err(AppError::IdentityChanged),
            None if hash(&source) == mapping.pre_adoption_hash => {
                atomic_write(&path, adopt_source(&source, &mapping.adopted_id).as_bytes())?;
                let mut generations = generations
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *generations
                    .entry(mapping.provisional_id.clone())
                    .or_default() += 1;
            }
            None => {
                return Err(AppError::ExternalConflict {
                    external_source: source,
                })
            }
        }
    }
    Ok(())
}

fn migrate_provisional_state(
    provisional: &Path,
    adopted: &Path,
    mappings: &[AdoptionMapping],
) -> Result<(), AppError> {
    migrate_recoveries(&provisional.join("recovery"), mappings)?;
    migrate_recoveries(&adopted.join("recovery"), mappings)?;
    let parent = adopted.parent().ok_or(AppError::PathEscape)?;
    fs::create_dir_all(parent)?;
    fs::create_dir_all(adopted)?;
    for entry in fs::read_dir(provisional)? {
        let source = entry?.path();
        if source.file_name().and_then(|value| value.to_str()) == Some("adoption.json") {
            continue;
        }
        let destination = adopted.join(source.file_name().ok_or(AppError::PathEscape)?);
        if destination.exists() {
            if matches!(
                source.file_name().and_then(|value| value.to_str()),
                Some("recovery" | "pending-native")
            ) {
                merge_recovery_directories(&source, &destination)?;
                fs::remove_dir(&source)?;
                continue;
            }
            return Err(AppError::DestinationExists(PathBuf::from(
                "adopted folder state entry",
            )));
        }
        fs::rename(source, destination)?;
    }
    sync_parent(provisional)?;
    sync_parent(adopted)?;
    sync_parent(parent)?;
    Ok(())
}

fn ensure_no_pending_native_drafts_for_adoption(directories: &[PathBuf]) -> Result<(), AppError> {
    for directory in directories {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let draft: PendingNativeDraftRecord = serde_json::from_slice(&fs::read(path)?)
                .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
            return Err(AppError::PendingNativeDraft(draft.note_id));
        }
    }
    Ok(())
}

fn migrate_recoveries(recovery: &Path, mappings: &[AdoptionMapping]) -> Result<(), AppError> {
    if recovery.is_dir() {
        for entry in fs::read_dir(recovery)? {
            let old_path = entry?.path();
            if old_path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let mut draft: RecoveryDraft = serde_json::from_slice(&fs::read(&old_path)?)
                .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
            let Some(mapping) = mappings
                .iter()
                .find(|mapping| mapping.relative_path == draft.relative_path)
            else {
                continue;
            };
            if draft.note_id != mapping.provisional_id && draft.note_id != mapping.adopted_id {
                continue;
            }
            draft.note_id = mapping.adopted_id.clone();
            if draft.base_hash == mapping.pre_adoption_hash {
                draft.base_hash = mapping.adopted_hash.clone();
            }
            draft.source = set_adopted_id(&draft.source, &mapping.adopted_id);
            let new_path = recovery.join(format!("{}.json", safe_key(&draft.note_id)));
            let bytes = serde_json::to_vec(&draft).map_err(io::Error::other)?;
            if new_path == old_path {
                atomic_write(&new_path, &bytes)?;
            } else {
                match create_no_replace(&new_path, &bytes) {
                    Ok(()) => {}
                    Err(AppError::DestinationExists(_)) if fs::read(&new_path)? == bytes => {}
                    Err(AppError::DestinationExists(_)) => {
                        return Err(AppError::DestinationExists(PathBuf::from(
                            "adopted recovery",
                        )))
                    }
                    Err(error) => return Err(error),
                }
                fs::remove_file(old_path)?;
            }
        }
    }
    Ok(())
}

fn merge_recovery_directories(source: &Path, destination: &Path) -> Result<(), AppError> {
    for entry in fs::read_dir(source)? {
        let source_file = entry?.path();
        let destination_file =
            destination.join(source_file.file_name().ok_or(AppError::PathEscape)?);
        match move_no_replace(&source_file, &destination_file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::read(&source_file)? == fs::read(&destination_file)? {
                    fs::remove_file(source_file)?;
                } else {
                    return Err(AppError::DestinationExists(PathBuf::from(
                        "adopted recovery",
                    )));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    sync_parent(source)?;
    sync_parent(destination)
}

fn complete_adoption_manifest(provisional: &Path) -> Result<(), AppError> {
    let manifest = provisional.join("adoption.json");
    if manifest.exists() {
        fs::remove_file(manifest)?;
        sync_parent(provisional)?;
    }
    match fs::remove_dir(provisional) {
        Ok(()) => {
            if let Some(parent) = provisional.parent() {
                sync_parent(parent)?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let has_entries = fs::read_dir(provisional)
                .ok()
                .and_then(|mut entries| entries.next())
                .is_some();
            if !has_entries {
                return Err(error.into());
            }
        }
    }
    Ok(())
}

fn adopt_source(source: &str, id: &str) -> String {
    if metadata(source).id.is_some() {
        return source.to_owned();
    }
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if let Some(end) = front_matter_end(source) {
        let closing_start = source[..end].rfind("---").unwrap();
        return format!(
            "{}howler_id: {}{}{}",
            &source[..closing_start],
            id,
            newline,
            &source[closing_start..]
        );
    }
    format!("---{newline}howler_id: {id}{newline}---{newline}{newline}{source}")
}

fn set_adopted_id(source: &str, id: &str) -> String {
    let Some(end) = front_matter_end(source) else {
        return adopt_source(source, id);
    };
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let first_line = source.find('\n').unwrap() + 1;
    let mut offset = first_line;
    for line in source[first_line..end].split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("howler_id:") {
            let leading = &line[..line.len() - trimmed.len()];
            return format!(
                "{}{leading}howler_id: {id}{newline}{}",
                &source[..offset],
                &source[offset + line.len()..]
            );
        }
        offset += line.len();
    }
    adopt_source(source, id)
}

fn rename_title_source(source: &str, title: &str) -> Result<String, AppError> {
    if title.contains(['\r', '\n']) {
        return Err(AppError::InvalidTitle);
    }
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if let Some(end) = front_matter_end(source) {
        let first_line = source.find('\n').unwrap() + 1;
        let mut offset = first_line;
        for line in source[first_line..end].split_inclusive('\n') {
            if line.trim_start().starts_with("title:") {
                let leading = &line[..line.len() - line.trim_start().len()];
                let serialized = yaml_scalar(title)?;
                return Ok(format!(
                    "{}{leading}title: {serialized}{newline}{}",
                    &source[..offset],
                    &source[offset + line.len()..]
                ));
            }
            offset += line.len();
        }
        let close = source[..end].rfind("---").unwrap();
        return Ok(format!(
            "{}title: {}{}{}",
            &source[..close],
            yaml_scalar(title)?,
            newline,
            &source[close..]
        ));
    }
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).starts_with("# ") {
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            return Ok(format!(
                "{}# {}{}{}",
                &source[..offset],
                title,
                ending,
                &source[offset + line.len()..]
            ));
        }
        offset += line.len();
    }
    Ok(format!("# {title}{newline}{newline}{source}"))
}

fn yaml_scalar(value: &str) -> Result<String, AppError> {
    let serialized = serde_yaml::to_string(value)
        .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
    Ok(serialized
        .strip_prefix("---\n")
        .unwrap_or(&serialized)
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    format_version: u32,
    library_id: String,
}

fn read_library_id(root: &Path) -> Result<Option<String>, AppError> {
    let path = validate_no_symlink_components(root, Path::new(".howler/library.json"), true)?;
    if !path.exists() {
        return Ok(None);
    }
    let library: LibraryFile = serde_json::from_slice(&fs::read(path)?)
        .map_err(|error| AppError::MalformedMetadata(error.to_string()))?;
    if library.format_version != 1 {
        return Err(AppError::MalformedMetadata(
            "unsupported library format version".into(),
        ));
    }
    validate_adopted_identity(&library.library_id)?;
    Ok(Some(library.library_id))
}

fn write_library(root: &Path, id: &str) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(&LibraryFile {
        format_version: 1,
        library_id: id.into(),
    })
    .map_err(io::Error::other)?;
    let directory = root.join(".howler");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(AppError::PathEscape)
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&directory)?,
        Err(error) => return Err(error.into()),
    }
    let path = validate_no_symlink_components(root, Path::new(".howler/library.json"), true)?;
    atomic_write(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use howler_editor::{HistoryHint, Replacement, Selection, TextRange};

    fn transaction(revision: u64, start: usize, end: usize, text: &str) -> Transaction {
        Transaction {
            expected_revision: revision,
            replacements: vec![Replacement {
                range: TextRange::new(start, end),
                text: text.into(),
            }],
            selections: vec![Selection::caret(start + text.len(), revision + 1)],
            history: HistoryHint::Typing,
        }
    }

    fn setup(adopt: bool) -> (tempfile::TempDir, tempfile::TempDir, NoteFolder) {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), adopt).unwrap();
        (notes, state, folder)
    }

    #[test]
    fn move_rejects_existing_destination_without_overwrite() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("a.md"), "A").unwrap();
        fs::write(notes.path().join("b.md"), "B").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("a.md"));
        assert!(matches!(
            folder.move_note(&id, "b.md"),
            Err(AppError::DestinationExists(_))
        ));
        assert_eq!(fs::read_to_string(notes.path().join("a.md")).unwrap(), "A");
        assert_eq!(fs::read_to_string(notes.path().join("b.md")).unwrap(), "B");
    }

    #[test]
    fn provisional_identity_survives_edit_save_search_and_reopen() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "# Before\n").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        let end = editor.snapshot().source.len();
        let outcome = editor.apply(transaction(0, end, end, "body")).unwrap();
        assert_eq!(outcome.durability, DurabilityState::RecoveryDurable);
        let revision = editor.snapshot().revision;
        folder.save_editor(&mut editor, revision).unwrap();
        drop(editor);
        assert_eq!(folder.open_editor(&id).unwrap().note_id().as_str(), id);
        assert_eq!(folder.search("body", 10).unwrap()[0].note.id.as_str(), id);
    }

    #[test]
    fn adopted_note_creation_replaces_imported_identity() {
        let (_notes, _state, folder) = setup(true);
        let original = folder.create_note(Some("body")).unwrap();
        let source = folder
            .open_editor(original.id.as_str())
            .unwrap()
            .snapshot()
            .source;
        let copy = folder.create_note(Some(&source)).unwrap();
        assert_ne!(copy.id, original.id);
        assert_eq!(folder.discover().unwrap().len(), 2);
    }

    #[test]
    fn duplicate_adopted_ids_reject_all_mutations() {
        let (notes, _state, folder) = setup(false);
        let id = Ulid::new().to_string();
        let source = format!("---\nhowler_id: {id}\n---\n# A\n");
        fs::write(notes.path().join("a.md"), &source).unwrap();
        fs::write(notes.path().join("b.md"), &source).unwrap();
        assert!(matches!(
            folder.open_editor(&id),
            Err(AppError::DuplicateIdentity(_))
        ));
        assert!(matches!(
            folder.trash(&id),
            Err(AppError::DuplicateIdentity(_))
        ));
        assert!(matches!(
            folder.move_note(&id, "c.md"),
            Err(AppError::DuplicateIdentity(_))
        ));
    }

    #[test]
    fn accepted_edit_and_undo_survive_recovery_failure() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "x").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        fs::remove_dir_all(folder.state_dir.join("recovery")).unwrap();
        fs::write(folder.state_dir.join("recovery"), "not a directory").unwrap();
        let outcome = editor.apply(transaction(0, 1, 1, "y")).unwrap();
        assert_eq!(outcome.edit.unwrap().revision, 1);
        assert_eq!(outcome.durability, DurabilityState::Accepted);
        assert!(outcome.recovery_error.is_some());
        let undo = editor.undo(1).unwrap();
        assert_eq!(undo.edit.unwrap().revision, 2);
        assert_eq!(undo.durability, DurabilityState::Accepted);
        assert_eq!(editor.snapshot().source, "x");
    }

    #[test]
    fn second_hash_validation_retains_recovery_and_external_file() {
        let (notes, _state, folder) = setup(false);
        let path = notes.path().join("note.md");
        fs::write(&path, "base").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        editor.apply(transaction(0, 4, 4, " local")).unwrap();
        let result =
            folder.save_editor_with_hook(&mut editor, || fs::write(&path, "external").unwrap());
        assert!(matches!(result, Err(AppError::ExternalConflict { .. })));
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");
        assert_eq!(folder.recoveries().unwrap().len(), 1);
    }

    #[test]
    fn save_reports_cleanup_and_transactional_index_state() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("# Search\nold body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        let source = editor.snapshot().source;
        let start = source.find("old").unwrap();
        editor
            .apply(transaction(0, start, start + 3, "new"))
            .unwrap();
        let revision = editor.snapshot().revision;
        let outcome = folder.save_editor(&mut editor, revision).unwrap();
        assert_eq!(outcome.durability, DurabilityState::FileSaved);
        assert_eq!(outcome.index_state, IndexState::Current);
        assert_eq!(outcome.recovery_cleanup, CleanupState::Removed);
        assert_eq!(folder.search("new", 10).unwrap().len(), 1);
        assert!(folder.search("old", 10).unwrap().is_empty());
    }

    #[test]
    fn canonical_save_reports_recovery_cleanup_failure_separately() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        fs::remove_file(&editor.recovery_path).unwrap();
        fs::create_dir(&editor.recovery_path).unwrap();
        let revision = editor.snapshot().revision;
        let outcome = folder.save_editor(&mut editor, revision).unwrap();
        assert_eq!(outcome.durability, DurabilityState::FileSaved);
        assert_eq!(outcome.recovery_cleanup, CleanupState::Retained);
        assert!(outcome.recovery_cleanup_error.is_some());
        assert_eq!(
            fs::read_to_string(&editor.absolute_path).unwrap(),
            "body draft"
        );
    }

    #[test]
    fn stale_index_is_recorded_and_retryable() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("# Retry\nbody")).unwrap();
        folder
            .mark_index_stale(&note.relative_path, "injected")
            .unwrap();
        assert_eq!(folder.retry_stale_indexes().unwrap(), 1);
        let count: i64 = folder
            .state
            .query_row("SELECT count(*) FROM stale_index", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn save_marks_real_index_failure_stale_then_retries() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("# Retry\nold")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        let end = editor.snapshot().source.len();
        editor.apply(transaction(0, end, end, " new")).unwrap();
        folder.index.execute("DROP TABLE note_fts", []).unwrap();
        let revision = editor.snapshot().revision;
        let outcome = folder.save_editor(&mut editor, revision).unwrap();
        assert_eq!(outcome.index_state, IndexState::Stale);
        let count: i64 = folder
            .state
            .query_row("SELECT count(*) FROM stale_index", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        initialize_index(&folder.index).unwrap();
        assert_eq!(folder.retry_stale_indexes().unwrap(), 1);
        assert_eq!(folder.search("new", 10).unwrap().len(), 1);
    }

    #[test]
    fn rebuild_rolls_back_deletion_when_an_insert_fails() {
        let (notes, _state, folder) = setup(false);
        folder.create_note(Some("# Existing\nsearchable")).unwrap();
        folder
            .index
            .execute_batch("CREATE TRIGGER reject_rebuild BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT, 'injected'); END;")
            .unwrap();
        fs::write(notes.path().join("external.md"), "new content").unwrap();
        assert!(folder.rebuild_index().is_err());
        folder
            .index
            .execute("DROP TRIGGER reject_rebuild", [])
            .unwrap();
        assert_eq!(folder.search("searchable", 10).unwrap().len(), 1);
        assert!(folder.search("new content", 10).unwrap().is_empty());
    }

    #[test]
    fn title_rename_preserves_crlf_and_quotes_yaml_safely() {
        let (notes, _state, folder) = setup(false);
        let source = "---\r\ncustom: yes\r\ntitle: old\r\n---\r\n\r\nbody\r\n";
        fs::write(notes.path().join("note.md"), source).unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        folder.rename_title(&id, "value: # special").unwrap();
        let changed = fs::read_to_string(notes.path().join("note.md")).unwrap();
        assert!(changed.contains("custom: yes\r\n"));
        assert!(!changed.replace("\r\n", "").contains('\n'));
        assert_eq!(derive_title(&changed), "value: # special");
        assert!(matches!(
            folder.rename_title(&id, "bad\ntitle"),
            Err(AppError::InvalidTitle)
        ));
    }

    #[test]
    fn wrong_owner_is_rejected() {
        let notes = tempfile::tempdir().unwrap();
        let state_a = tempfile::tempdir().unwrap();
        let state_b = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        let folder_a = NoteFolder::open(notes.path(), state_a.path(), false).unwrap();
        let folder_b = NoteFolder::open(notes.path(), state_b.path(), false).unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder_a.open_editor(&id).unwrap();
        let revision = editor.snapshot().revision;
        assert!(matches!(
            folder_b.save_editor(&mut editor, revision),
            Err(AppError::WrongOwner)
        ));
    }

    #[test]
    fn folder_contexts_share_per_note_operation_executors() {
        let notes = tempfile::tempdir().unwrap();
        let state_a = tempfile::tempdir().unwrap();
        let state_b = tempfile::tempdir().unwrap();
        let folder_a = NoteFolder::open(notes.path(), state_a.path(), false).unwrap();
        let folder_b = NoteFolder::open(notes.path(), state_b.path(), false).unwrap();
        assert!(Arc::ptr_eq(
            &folder_a.operation_locks,
            &folder_b.operation_locks
        ));
        assert!(Arc::ptr_eq(&folder_a.generations, &folder_b.generations));
    }

    #[test]
    fn all_editor_mutations_and_save_use_shared_per_note_executor() {
        use std::sync::mpsc;
        use std::time::Duration;

        fn serialized(
            mut editor: NoteEditor,
            operation: impl FnOnce(&mut NoteEditor) + Send + 'static,
        ) -> NoteEditor {
            let lock = Arc::clone(&editor.operation_lock);
            let guard = lock.lock().unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let (finished_tx, finished_rx) = mpsc::channel();
            std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                operation(&mut editor);
                finished_tx.send(editor).unwrap();
            });
            started_rx.recv().unwrap();
            assert!(finished_rx.recv_timeout(Duration::from_millis(20)).is_err());
            drop(guard);
            finished_rx.recv_timeout(Duration::from_secs(1)).unwrap()
        }

        let notes = tempfile::tempdir().unwrap();
        let state_a = tempfile::tempdir().unwrap();
        let state_b = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        let folder_a = NoteFolder::open(notes.path(), state_a.path(), false).unwrap();
        let folder_b = NoteFolder::open(notes.path(), state_b.path(), false).unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder_a.open_editor(&id).unwrap();
        assert!(matches!(
            folder_b.open_editor(&id),
            Err(AppError::NoteAlreadyOpen(_))
        ));

        editor = serialized(editor, |editor| {
            editor.apply(transaction(0, 4, 4, "!")).unwrap();
        });
        editor = serialized(editor, |editor| {
            editor
                .execute_command(
                    1,
                    EditorCommand::Bold {
                        range: TextRange::new(0, 4),
                    },
                )
                .unwrap();
        });
        editor = serialized(editor, |editor| {
            editor.undo(2).unwrap();
        });
        editor = serialized(editor, |editor| {
            editor.redo(3).unwrap();
        });
        fs::write(&editor.absolute_path, "external").unwrap();
        editor = serialized(editor, |editor| {
            assert!(matches!(
                editor.reconcile_external().unwrap(),
                ReconcileResult::Conflict { .. }
            ));
        });
        fs::write(&editor.absolute_path, "body").unwrap();

        let lock = Arc::clone(&editor.operation_lock);
        let guard = lock.lock().unwrap();
        let revision = editor.snapshot().revision;
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = folder_a.save_editor(&mut editor, revision);
            finished_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        assert!(finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_ok());
    }

    #[test]
    fn rename_holds_same_note_executor_for_entire_workflow() {
        use std::sync::mpsc;
        use std::time::Duration;

        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "# Before\n").unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        let id = provisional_id(Path::new("note.md"));
        let lock = folder.note_lock(&id);
        let guard = lock.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            finished_tx.send(folder.rename_title(&id, "After")).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        let renamed = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(renamed.title, "After");
        assert!(fs::read_to_string(notes.path().join("note.md"))
            .unwrap()
            .contains("# After"));
    }

    #[test]
    fn save_rejects_stale_revision_without_touching_file() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "body").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        assert!(matches!(
            folder.save_editor(&mut editor, 0),
            Err(AppError::Editor(EditorError::StaleRevision { .. }))
        ));
        assert_eq!(
            fs::read_to_string(notes.path().join("note.md")).unwrap(),
            "body"
        );
    }

    #[test]
    fn fuzzy_search_and_recent_order_are_deterministic() {
        let (_notes, _state, folder) = setup(false);
        let first = folder.create_note(Some("# Project Alpha\nbody")).unwrap();
        let second = folder.create_note(Some("# Project Beta\nbody")).unwrap();
        folder.open_editor(first.id.as_str()).unwrap();
        let fuzzy = folder.search("prjalp", 10).unwrap();
        assert_eq!(fuzzy[0].reason, MatchReason::FuzzyTitle);
        let recent = folder.search("", 10).unwrap();
        assert_eq!(recent[0].note.id.as_str(), first.id.as_str());
        assert_eq!(recent[1].note.id.as_str(), second.id.as_str());
    }

    #[test]
    fn operational_state_persists_cursor_outside_note_folder() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "body").unwrap();
        folder.set_cursor(Path::new("note.md"), 3).unwrap();
        assert_eq!(folder.cursor(Path::new("note.md")).unwrap(), Some(3));
        assert!(folder.state_dir.join("state.sqlite3").exists());
        assert!(!notes.path().join("state.sqlite3").exists());
    }

    #[test]
    fn recovery_restore_and_rescan_are_actionable() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "body").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        drop(editor);
        let restored = folder.restore_recovery(&id).unwrap();
        assert_eq!(restored.snapshot().source, "body draft");
        fs::write(notes.path().join("external.md"), "new file").unwrap();
        assert_eq!(folder.rescan().unwrap().notes, 2);
    }

    #[test]
    fn reconciliation_refreshes_clean_editor_and_preserves_dirty_conflict() {
        let (notes, _state, folder) = setup(false);
        let path = notes.path().join("note.md");
        fs::write(&path, "base").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut clean = folder.open_editor(&id).unwrap();
        fs::write(&path, "external").unwrap();
        assert!(matches!(
            clean.reconcile_external().unwrap(),
            ReconcileResult::Refreshed { .. }
        ));
        assert_eq!(clean.snapshot().source, "external");
        clean.apply(transaction(1, 8, 8, " local")).unwrap();
        fs::write(&path, "second external").unwrap();
        assert!(matches!(
            clean.reconcile_external().unwrap(),
            ReconcileResult::Conflict { .. }
        ));
        assert_eq!(clean.snapshot().source, "external local");
    }

    #[test]
    fn deleting_index_preserves_state_and_recovery() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let id;
        let index_path;
        {
            let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
            let note = folder.create_note(Some("# Durable\nsearch body")).unwrap();
            id = note.id.as_str().to_owned();
            folder.set_cursor(&note.relative_path, 3).unwrap();
            let mut editor = folder.open_editor(&id).unwrap();
            let end = editor.snapshot().source.len();
            editor.apply(transaction(0, end, end, " draft")).unwrap();
            index_path = folder.state_dir.join("index.sqlite3");
        }
        fs::remove_file(&index_path).unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        assert_eq!(
            folder.cursor(Path::new("untitled-placeholder.md")).unwrap(),
            None
        );
        let note = folder.find_unique_note(&id).unwrap();
        assert_eq!(folder.cursor(&note.relative_path).unwrap(), Some(3));
        assert_eq!(folder.recoveries().unwrap().len(), 1);
        assert_eq!(folder.search("search", 10).unwrap().len(), 1);
    }

    #[test]
    fn adoption_publishes_library_after_rewrites() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body\r\n").unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), true).unwrap();
        assert!(folder.is_adopted());
        assert!(notes.path().join(".howler/library.json").exists());
        let source = fs::read_to_string(notes.path().join("note.md")).unwrap();
        assert!(source.contains("howler_id:"));
        assert!(source.contains("\r\n"));
    }

    #[test]
    fn failed_adoption_does_not_publish_library_identity() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("bad.md"), [0xff, 0xfe]).unwrap();
        assert!(matches!(
            NoteFolder::open(notes.path(), state.path(), true),
            Err(AppError::InvalidUtf8(_))
        ));
        assert!(!notes.path().join(".howler/library.json").exists());
    }

    #[test]
    fn restored_recovery_conflicts_when_canonical_changed() {
        let (notes, _state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "base").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        fs::write(notes.path().join("note.md"), "external").unwrap();
        drop(editor);

        let mut restored = folder.restore_recovery(&id).unwrap();
        let revision = restored.snapshot().revision;
        assert!(matches!(
            folder.save_editor(&mut restored, revision),
            Err(AppError::ExternalConflict { .. })
        ));
        assert_eq!(
            fs::read_to_string(notes.path().join("note.md")).unwrap(),
            "external"
        );
    }

    #[test]
    fn pending_recovery_blocks_normal_open_until_discarded() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        drop(editor);
        assert!(matches!(
            folder.open_editor(note.id.as_str()),
            Err(AppError::RecoveryPending(_))
        ));
        folder.discard_recovery(note.id.as_str()).unwrap();
        folder.open_editor(note.id.as_str()).unwrap();
    }

    #[test]
    fn invalid_and_changed_adopted_ids_are_rejected() {
        let (notes, _state, folder) = setup(true);
        fs::write(
            notes.path().join("invalid.md"),
            "---\nhowler_id: user-chosen\n---\nbody",
        )
        .unwrap();
        assert!(matches!(
            folder.discover(),
            Err(AppError::MalformedMetadata(_))
        ));
        fs::remove_file(notes.path().join("invalid.md")).unwrap();

        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        let source = editor.snapshot().source;
        let start = source.find(note.id.as_str()).unwrap();
        editor
            .apply(transaction(
                0,
                start,
                start + note.id.as_str().len(),
                &Ulid::new().to_string(),
            ))
            .unwrap();
        let revision = editor.snapshot().revision;
        assert!(matches!(
            folder.save_editor(&mut editor, revision),
            Err(AppError::IdentityChanged)
        ));
    }

    #[test]
    fn adoption_migrates_provisional_recovery() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        {
            let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
            let id = provisional_id(Path::new("note.md"));
            let mut editor = folder.open_editor(&id).unwrap();
            editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        }

        let folder = NoteFolder::open(notes.path(), state.path(), true).unwrap();
        let drafts = folder.recoveries().unwrap();
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].note_id.len(), 26);
        assert!(drafts[0].source.contains("body draft"));
        assert!(drafts[0].source.contains(&drafts[0].note_id));
        assert_eq!(
            folder
                .restore_recovery(&drafts[0].note_id)
                .unwrap()
                .snapshot()
                .source,
            drafts[0].source
        );
    }

    #[test]
    fn adoption_rejects_and_preserves_pending_native_draft_metadata() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        {
            let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
            let id = provisional_id(Path::new("note.md"));
            let editor = folder.open_editor(&id).unwrap();
            editor
                .persist_pending_native_draft(&PendingNativeDraft {
                    base_revision: 0,
                    source: "body pending".into(),
                })
                .unwrap();
        }

        assert!(matches!(
            NoteFolder::open(notes.path(), state.path(), true),
            Err(AppError::PendingNativeDraft(_))
        ));
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        let pending = folder.pending_native_drafts().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].note_id.len(), 64);
        assert_eq!(pending[0].source, "body pending");
    }

    #[test]
    fn adoption_preserves_conflict_when_recovery_base_predates_external_edit() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = notes.path().join("note.md");
        fs::write(&path, "base").unwrap();
        {
            let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
            let id = provisional_id(Path::new("note.md"));
            let mut editor = folder.open_editor(&id).unwrap();
            editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        }
        fs::write(&path, "external").unwrap();

        let folder = NoteFolder::open(notes.path(), state.path(), true).unwrap();
        let draft = folder.recoveries().unwrap().pop().unwrap();
        assert_eq!(draft.base_hash, hash("base"));
        assert!(draft.source.contains(&draft.note_id));
        let mut editor = folder.restore_recovery(&draft.note_id).unwrap();
        let revision = editor.snapshot().revision;
        assert!(matches!(
            folder.save_editor(&mut editor, revision),
            Err(AppError::ExternalConflict { .. })
        ));
    }

    #[test]
    fn adoption_manifest_reuses_ids_after_partial_rewrite_and_migration() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("a.md"), "a").unwrap();
        fs::write(notes.path().join("b.md"), "b").unwrap();
        let root = fs::canonicalize(notes.path()).unwrap();
        let provisional = state
            .path()
            .join("folders")
            .join(provisional_folder_id(&root));
        let manifest = load_or_create_adoption_manifest(&root, &provisional, None).unwrap();
        let mapping = &manifest.mappings[0];
        let recovery = provisional.join("recovery");
        fs::create_dir(&recovery).unwrap();
        let draft = RecoveryDraft {
            note_id: mapping.provisional_id.clone(),
            relative_path: mapping.relative_path.clone(),
            revision: 1,
            base_hash: mapping.pre_adoption_hash.clone(),
            source: format!(
                "{} draft",
                fs::read_to_string(root.join(&mapping.relative_path)).unwrap()
            ),
        };
        atomic_write(
            &recovery.join(format!("{}.json", safe_key(&draft.note_id))),
            &serde_json::to_vec(&draft).unwrap(),
        )
        .unwrap();
        let generations = ApplicationServices::shared()
            .runtime(&root)
            .generations
            .clone();
        apply_adoption_manifest(&root, &manifest, &generations).unwrap();
        apply_adoption_manifest(&root, &manifest, &generations).unwrap();
        let reloaded = load_or_create_adoption_manifest(&root, &provisional, None).unwrap();
        assert_eq!(
            manifest
                .mappings
                .iter()
                .map(|mapping| &mapping.adopted_id)
                .collect::<Vec<_>>(),
            reloaded
                .mappings
                .iter()
                .map(|mapping| &mapping.adopted_id)
                .collect::<Vec<_>>()
        );
        let adopted = state.path().join("folders").join(&manifest.library_id);
        migrate_provisional_state(&provisional, &adopted, &manifest.mappings).unwrap();
        migrate_provisional_state(&provisional, &adopted, &manifest.mappings).unwrap();
        assert!(provisional.join("adoption.json").exists());
        let migrated: RecoveryDraft = serde_json::from_slice(
            &fs::read(
                adopted
                    .join("recovery")
                    .join(format!("{}.json", safe_key(&mapping.adopted_id))),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(migrated.note_id, mapping.adopted_id);
        assert_eq!(migrated.base_hash, mapping.adopted_hash);
        assert!(migrated.source.contains(&mapping.adopted_id));
    }

    #[test]
    fn adoption_rewrite_invalidates_provisional_editor() {
        let (notes, state, folder) = setup(false);
        fs::write(notes.path().join("note.md"), "body").unwrap();
        folder.rebuild_index().unwrap();
        let id = provisional_id(Path::new("note.md"));
        let mut editor = folder.open_editor(&id).unwrap();
        let provisional = state
            .path()
            .join("folders")
            .join(provisional_folder_id(folder.root()));
        let manifest = load_or_create_adoption_manifest(folder.root(), &provisional, None).unwrap();
        apply_adoption_manifest(folder.root(), &manifest, &folder.generations).unwrap();
        assert!(matches!(
            editor.apply(transaction(0, 4, 4, " draft")),
            Err(AppError::StaleHandle)
        ));
    }

    #[test]
    fn create_is_exclusive_and_never_replaces_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "existing").unwrap();
        assert!(matches!(
            create_no_replace(&path, b"replacement"),
            Err(AppError::DestinationExists(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "existing");
    }

    #[test]
    fn pending_recovery_rejects_move_and_trash() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        assert!(matches!(
            folder.move_note(note.id.as_str(), "moved.md"),
            Err(AppError::RecoveryPending(_))
        ));
        assert!(matches!(
            folder.trash(note.id.as_str()),
            Err(AppError::RecoveryPending(_))
        ));
    }

    #[test]
    fn committed_lifecycle_operations_ignore_index_bookkeeping_failure() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        folder.index.execute("DROP TABLE note_fts", []).unwrap();
        let moved = folder.move_note(note.id.as_str(), "moved.md").unwrap();
        let trashed = folder.trash(moved.id.as_str()).unwrap();
        let restored = folder.restore(trashed).unwrap();
        assert_eq!(restored.title, "body");
    }

    #[test]
    fn parent_sync_failure_retains_recovery_and_reports_uncertain_durability() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        editor.apply(transaction(0, 4, 4, " draft")).unwrap();
        let outcome = folder.save_editor_with_sync_failure(&mut editor).unwrap();
        assert_eq!(outcome.durability, DurabilityState::RecoveryDurable);
        assert!(outcome.canonical_error.is_some());
        assert_eq!(outcome.recovery_cleanup, CleanupState::Retained);
        assert_eq!(folder.recoveries().unwrap().len(), 1);
    }

    #[test]
    fn moved_note_invalidates_existing_editor() {
        let (_notes, _state, folder) = setup(false);
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        folder.move_note(note.id.as_str(), "moved.md").unwrap();
        assert!(matches!(
            editor.apply(transaction(0, 4, 4, " draft")),
            Err(AppError::StaleHandle)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn mutations_reject_symlinked_ancestors_and_trash() {
        use std::os::unix::fs::symlink;

        let (notes, state, folder) = setup(false);
        let outside = tempfile::tempdir().unwrap();
        let note = folder.create_note(Some("body")).unwrap();
        symlink(outside.path(), notes.path().join("linked")).unwrap();
        assert!(matches!(
            folder.move_note(note.id.as_str(), "linked/escaped.md"),
            Err(AppError::PathEscape)
        ));
        symlink(outside.path(), notes.path().join(".trash")).unwrap();
        assert!(matches!(
            folder.trash(note.id.as_str()),
            Err(AppError::PathEscape)
        ));
        assert!(!outside.path().join("escaped.md").exists());

        let adoption_notes = tempfile::tempdir().unwrap();
        fs::write(adoption_notes.path().join("note.md"), "body").unwrap();
        symlink(outside.path(), adoption_notes.path().join(".howler")).unwrap();
        assert!(matches!(
            NoteFolder::open(adoption_notes.path(), state.path(), true),
            Err(AppError::PathEscape)
        ));
    }

    fn connect_session(notes: &tempfile::TempDir, state: &tempfile::TempDir) -> ApplicationSession {
        let mut session = ApplicationSession::default();
        let response = session.connect(ConnectFolder {
            folder_path: notes.path().into(),
            application_state_path: state.path().into(),
            adopt: false,
            create_missing: false,
        });
        assert!(matches!(response.outcome, OperationOutcome::Applied(_)));
        session
    }

    #[test]
    fn application_session_connects_empty_folder_with_coherent_editor_state() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let session = connect_session(&notes, &state);
        let active = session.state().active.unwrap();
        assert_eq!(
            active.editor.snapshot.revision,
            active.editor.decorations.revision
        );
        assert_eq!(
            active.persistence.replacement_safety,
            ReplacementSafety::Safe
        );
        assert_eq!(session.state().recoveries.len(), 0);
    }

    #[test]
    fn application_session_returns_structured_stale_revision_and_current_state() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        let response = session.apply_text_edit(HostTextEdit {
            expected_revision: 9,
            replacements: Vec::new(),
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Isolated,
            composition: None,
            input_origin: None,
        });
        let OperationOutcome::Rejected(problem) = response.outcome else {
            panic!("stale edit was applied");
        };
        assert!(matches!(problem.code, ProblemCode::StaleRevision));
        assert!(matches!(
            problem.details,
            Some(ProblemDetails::StaleRevision {
                expected_revision: 9,
                current_revision: 0
            })
        ));
        assert_eq!(response.state.active.unwrap().editor.snapshot.revision, 0);
    }

    #[test]
    fn application_session_selection_updates_cursor_without_editing_or_history() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        let edited = session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: TextRange::new(0, 0),
                text: "a😀b".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: Some(InputOrigin::Typing),
        });
        assert!(matches!(edited.outcome, OperationOutcome::Applied(_)));
        let active = edited.state.active.unwrap();
        let relative_path = session
            .folder_ref()
            .unwrap()
            .discover()
            .unwrap()
            .into_iter()
            .find(|note| note.id == active.note_id)
            .unwrap()
            .relative_path;
        let reversed = Selection {
            anchor: 5,
            head: 1,
            affinity: howler_editor::Affinity::Upstream,
            revision: 1,
        };

        let response = session.apply_selection(HostSelectionUpdate {
            expected_revision: 1,
            selections: vec![reversed.clone()],
        });
        let OperationOutcome::Applied(result) = response.outcome else {
            panic!("selection update was rejected");
        };
        assert_eq!(result.revision, 1);
        assert_eq!(result.selections, vec![reversed.clone()]);
        let snapshot = response.state.active.unwrap().editor.snapshot;
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.source, "a😀b");
        assert!(snapshot.can_undo);
        assert_eq!(
            session
                .folder_ref()
                .unwrap()
                .cursor(&relative_path)
                .unwrap(),
            Some(1)
        );

        let invalid = session.apply_selection(HostSelectionUpdate {
            expected_revision: 1,
            selections: vec![Selection::caret(2, 1)],
        });
        assert!(matches!(
            invalid.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::InvalidOperation,
                ..
            })
        ));
        let stale = session.apply_selection(HostSelectionUpdate {
            expected_revision: 0,
            selections: vec![Selection::caret(0, 0)],
        });
        assert!(matches!(
            stale.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::StaleRevision,
                ..
            })
        ));

        let undone = session.undo(1);
        assert!(matches!(undone.outcome, OperationOutcome::Applied(Some(_))));
        assert_eq!(undone.state.active.unwrap().editor.snapshot.source, "");
    }

    #[test]
    fn application_session_reports_required_native_host_capabilities() {
        let session = ApplicationSession::default();
        let OperationOutcome::Applied(capabilities) = session.capabilities().outcome else {
            panic!("capabilities were rejected");
        };
        assert_eq!(capabilities.application_session_abi, 2);
        assert!(capabilities.selection_updates);
        assert!(capabilities.input_origin_metadata);
        assert!(capabilities.rust_owned_history);
        assert!(capabilities.pending_native_drafts);
    }

    #[test]
    fn second_session_cannot_open_or_submit_revision_zero_for_active_note() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let services = ApplicationServices::new();
        let request = ConnectFolder {
            folder_path: notes.path().into(),
            application_state_path: state.path().into(),
            adopt: false,
            create_missing: false,
        };
        let mut first = ApplicationSession::new(Arc::clone(&services));
        let first_connected = first.connect(request.clone());
        let note_id = first_connected.state.active.unwrap().note_id;
        let mut second = ApplicationSession::new(services);
        let second_connected = second.connect(request);
        assert!(matches!(
            second_connected.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::InvalidOperation,
                ..
            })
        ));
        assert!(second_connected.state.active.is_none());
        assert!(matches!(
            second.open_note(note_id.as_str()).outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::InvalidOperation,
                ..
            })
        ));

        let first_edit = first.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: TextRange::new(0, 0),
                text: "first".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        assert!(matches!(first_edit.outcome, OperationOutcome::Applied(_)));
        assert!(matches!(
            second.open_note(note_id.as_str()).outcome,
            OperationOutcome::Rejected(_)
        ));
    }

    #[test]
    fn editor_keeps_runtime_lease_alive_after_folder_drop() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        let id = provisional_id(Path::new("note.md"));
        let editor = folder.open_editor(&id).unwrap();
        drop(folder);

        let reopened = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        assert!(matches!(
            reopened.open_editor(&id),
            Err(AppError::NoteAlreadyOpen(_))
        ));
        drop(editor);
        assert!(reopened.open_editor(&id).is_ok());
    }

    #[test]
    fn moving_active_adopted_note_reuses_editor_and_lease() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        let adopted = NoteFolder::open(notes.path(), state.path(), true).unwrap();
        let id = adopted.discover().unwrap()[0].id.clone();
        drop(adopted);
        let mut session = connect_session(&notes, &state);

        let moved = session.move_note(MoveNote {
            note_id: id.as_str().to_owned(),
            destination: PathBuf::from("nested/moved.md"),
        });
        let OperationOutcome::Applied(result) = moved.outcome else {
            panic!("active adopted move was rejected");
        };
        assert_eq!(result.note.id, id);
        assert_eq!(result.note.relative_path, Path::new("nested/moved.md"));
        let active = session.state().active.unwrap();
        assert_eq!(active.note_id, id);
        assert!(active.editor.snapshot.source.contains("body"));
        assert!(active.editor.snapshot.source.contains(id.as_str()));
        let second_folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        assert!(matches!(
            second_folder.open_editor(id.as_str()),
            Err(AppError::NoteAlreadyOpen(_))
        ));
        drop(second_folder);
        assert!(matches!(
            session
                .rename_note(RenameNote {
                    note_id: id.as_str().to_owned(),
                    title: "Moved title".into(),
                })
                .outcome,
            OperationOutcome::Applied(_)
        ));
        assert_eq!(
            derive_title(&fs::read_to_string(notes.path().join("nested/moved.md")).unwrap()),
            "Moved title"
        );
    }

    #[test]
    fn application_session_rejects_stale_identified_autosave() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        let response = session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: howler_editor::TextRange::new(0, 0),
                text: "draft".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        let target = response
            .effects
            .iter()
            .find_map(|effect| match effect {
                HostEffect::ScheduleAutosave { target, .. } => Some(target.clone()),
                _ => None,
            })
            .unwrap();
        let created = session.create_note(CreateNote { source: None });
        assert!(matches!(created.outcome, OperationOutcome::Applied(_)));
        let stale = session.save(target);
        assert!(matches!(
            stale.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::StaleEditor,
                ..
            })
        ));
        assert_ne!(
            stale.state.active.unwrap().note_id,
            response.state.active.unwrap().note_id
        );
    }

    #[test]
    fn accepted_only_edit_prevents_replacement() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        let recovery = session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .state_dir
            .join("recovery");
        fs::remove_dir_all(&recovery).unwrap();
        fs::write(&recovery, "not a directory").unwrap();
        let edited = session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: howler_editor::TextRange::new(0, 0),
                text: "unsafe".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        assert_eq!(
            edited.state.active.unwrap().persistence.replacement_safety,
            ReplacementSafety::MustRetainEditor
        );
        let replacement = session.create_note(CreateNote { source: None });
        assert!(matches!(
            replacement.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::PersistenceFailure,
                ..
            })
        ));
        assert!(replacement.state.active.is_some());
    }

    #[test]
    fn pending_native_draft_survives_autosave_and_blocks_replacement_until_resolution() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        let original_id = session.state().active.unwrap().note_id;
        let edited = session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: howler_editor::TextRange::new(0, 0),
                text: "active".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        let (effect_id, target) = edited
            .effects
            .iter()
            .find_map(|effect| match effect {
                HostEffect::ScheduleAutosave {
                    effect_id, target, ..
                } => Some((effect_id.clone(), target.clone())),
                _ => None,
            })
            .unwrap();
        let preserved = session.preserve_pending_native_draft(PendingNativeDraft {
            base_revision: 1,
            source: "native committed input".into(),
        });
        assert!(preserved.effects.iter().any(|effect| matches!(
            effect,
            HostEffect::CancelEffect { effect_id: cancelled } if cancelled == &effect_id
        )));
        let pending = preserved
            .state
            .active
            .as_ref()
            .unwrap()
            .pending_native_draft
            .as_ref()
            .unwrap();
        assert!(pending.durable);
        assert_eq!(
            preserved
                .state
                .active
                .unwrap()
                .persistence
                .replacement_safety,
            ReplacementSafety::MustRetainEditor
        );

        let saved = session.save(target);
        assert!(matches!(saved.outcome, OperationOutcome::Applied(_)));
        assert!(saved
            .state
            .active
            .as_ref()
            .unwrap()
            .pending_native_draft
            .is_some());
        assert_eq!(
            saved
                .state
                .active
                .as_ref()
                .unwrap()
                .persistence
                .replacement_safety,
            ReplacementSafety::MustRetainEditor
        );
        let persisted = session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .pending_native_drafts()
            .unwrap();
        assert_eq!(persisted[0].source, "native committed input");

        let replaced = session.create_note(CreateNote { source: None });
        assert!(matches!(replaced.outcome, OperationOutcome::Rejected(_)));
        assert_eq!(replaced.state.active.unwrap().note_id, original_id);

        let resolved = session.resolve_pending_native_draft(PendingDraftResolution::Discard);
        assert!(matches!(resolved.outcome, OperationOutcome::Applied(_)));
        assert!(resolved
            .state
            .active
            .as_ref()
            .unwrap()
            .pending_native_draft
            .is_none());
        assert!(session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .pending_native_drafts()
            .unwrap()
            .is_empty());
        assert!(matches!(
            session.create_note(CreateNote { source: None }).outcome,
            OperationOutcome::Applied(_)
        ));
    }

    #[test]
    fn pending_native_draft_is_reloaded_with_active_recovery_after_restart() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let note_id = {
            let mut session = connect_session(&notes, &state);
            session.apply_text_edit(HostTextEdit {
                expected_revision: 0,
                replacements: vec![howler_editor::Replacement {
                    range: howler_editor::TextRange::new(0, 0),
                    text: "active recovery".into(),
                }],
                selections: Vec::new(),
                history: howler_editor::HistoryHint::Typing,
                composition: None,
                input_origin: None,
            });
            let note_id = session.state().active.unwrap().note_id;
            session.preserve_pending_native_draft(PendingNativeDraft {
                base_revision: 1,
                source: "pending native".into(),
            });
            note_id
        };

        let mut session = connect_session(&notes, &state);
        let active = session.state().active.unwrap();
        assert_eq!(active.note_id, note_id);
        assert_eq!(active.editor.snapshot.source, "active recovery");
        assert_eq!(
            active.pending_native_draft.unwrap().source,
            "pending native"
        );
        assert_eq!(
            active.persistence.replacement_safety,
            ReplacementSafety::MustRetainEditor
        );
        let resolved = session.resolve_pending_native_draft(PendingDraftResolution::Discard);
        assert!(matches!(resolved.outcome, OperationOutcome::Applied(_)));
        assert!(resolved
            .state
            .recoveries
            .iter()
            .any(|draft| draft.note_id == note_id.as_str() && draft.source == "active recovery"));
        assert!(session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .pending_native_drafts()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pending_resolution_failure_keeps_pending_state_and_file() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: howler_editor::TextRange::new(0, 0),
                text: "active".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        session.preserve_pending_native_draft(PendingNativeDraft {
            base_revision: 1,
            source: "pending".into(),
        });
        let recovery = session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .state_dir
            .join("recovery");
        fs::remove_dir_all(&recovery).unwrap();
        fs::write(&recovery, "not a directory").unwrap();

        let response = session.resolve_pending_native_draft(PendingDraftResolution::Discard);
        assert!(matches!(
            response.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::PersistenceFailure,
                ..
            })
        ));
        assert!(response
            .state
            .active
            .unwrap()
            .pending_native_draft
            .is_some());
        assert_eq!(
            session
                .folder
                .as_ref()
                .unwrap()
                .folder
                .pending_native_drafts()
                .unwrap()[0]
                .source,
            "pending"
        );
    }

    #[test]
    fn pending_save_as_new_retry_is_idempotent_after_cleanup_failure() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        session.preserve_pending_native_draft(PendingNativeDraft {
            base_revision: 0,
            source: "save this pending input".into(),
        });
        let pending_path = session
            .active
            .as_ref()
            .unwrap()
            .editor
            .pending_native_draft_path
            .clone();
        fs::remove_file(&pending_path).unwrap();
        fs::create_dir(&pending_path).unwrap();
        session
            .folder
            .as_ref()
            .unwrap()
            .folder
            .index
            .execute("DROP TABLE note_fts", [])
            .unwrap();
        let resolution = PendingDraftResolution::SaveAsNew {
            operation_id: "pending-save-1".into(),
            title: Some("Saved pending".into()),
        };
        let first = session.resolve_pending_native_draft(resolution.clone());
        assert!(matches!(
            first.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::PersistenceFailure,
                ..
            })
        ));
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);

        fs::remove_dir(&pending_path).unwrap();
        let second = session.resolve_pending_native_draft(resolution);
        assert!(matches!(second.outcome, OperationOutcome::Applied(_)));
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);
    }

    #[test]
    fn pending_save_as_new_post_success_retry_returns_same_note() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut session = connect_session(&notes, &state);
        session.preserve_pending_native_draft(PendingNativeDraft {
            base_revision: 0,
            source: "pending source".into(),
        });
        let resolution = PendingDraftResolution::SaveAsNew {
            operation_id: "pending-post-success".into(),
            title: Some("Pending copy".into()),
        };
        let first = session.resolve_pending_native_draft(resolution.clone());
        let OperationOutcome::Applied(first) = first.outcome else {
            panic!("first pending resolution was rejected");
        };
        let second = session.resolve_pending_native_draft(resolution.clone());
        let OperationOutcome::Applied(second) = second.outcome else {
            panic!("post-success pending retry was rejected");
        };
        assert_eq!(second.note.id, first.note.id);
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);

        session.preserve_pending_native_draft(PendingNativeDraft {
            base_revision: 0,
            source: "different pending source".into(),
        });
        assert!(matches!(
            session.resolve_pending_native_draft(resolution).outcome,
            OperationOutcome::Rejected(_)
        ));
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);
    }

    #[test]
    fn pending_native_draft_blocks_all_external_note_lifecycle_paths() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let note_id = {
            let mut session = connect_session(&notes, &state);
            let note_id = session.state().active.unwrap().note_id;
            session.preserve_pending_native_draft(PendingNativeDraft {
                base_revision: 0,
                source: "pending".into(),
            });
            note_id
        };
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        assert!(matches!(
            folder.open_editor(note_id.as_str()),
            Err(AppError::PendingNativeDraft(_))
        ));
        assert!(matches!(
            folder.rename_title(note_id.as_str(), "renamed"),
            Err(AppError::PendingNativeDraft(_))
        ));
        assert!(matches!(
            folder.move_note(note_id.as_str(), "moved.md"),
            Err(AppError::PendingNativeDraft(_))
        ));
        assert!(matches!(
            folder.trash(note_id.as_str()),
            Err(AppError::PendingNativeDraft(_))
        ));
        drop(folder);
        assert!(matches!(
            NoteFolder::open(notes.path(), state.path(), true),
            Err(AppError::PendingNativeDraft(_))
        ));
        let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
        assert_eq!(folder.pending_native_drafts().unwrap()[0].source, "pending");
    }

    #[test]
    fn connect_opens_note_without_recovery_when_another_note_has_one() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let recovered_id;
        let unrelated_id;
        {
            let folder = NoteFolder::open(notes.path(), state.path(), false).unwrap();
            let recovered = folder.create_note(Some("recovered")).unwrap();
            let unrelated = folder.create_note(Some("unrelated")).unwrap();
            recovered_id = recovered.id;
            unrelated_id = unrelated.id;
            let mut editor = folder.open_editor(recovered_id.as_str()).unwrap();
            editor
                .apply(transaction(
                    0,
                    "recovered".len(),
                    "recovered".len(),
                    " draft",
                ))
                .unwrap();
        }

        let session = connect_session(&notes, &state);
        let active = session.state().active.unwrap();
        assert_eq!(active.note_id, unrelated_id);
        assert!(session
            .state()
            .recoveries
            .iter()
            .any(|draft| draft.note_id == recovered_id.as_str()));
    }

    #[test]
    fn reconciliation_rejects_changed_identity_without_publishing_source() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let folder = NoteFolder::open(notes.path(), state.path(), true).unwrap();
        let note = folder.create_note(Some("body")).unwrap();
        let mut editor = folder.open_editor(note.id.as_str()).unwrap();
        let original = editor.snapshot();
        let changed = set_adopted_id(&original.source, &Ulid::new().to_string());
        fs::write(notes.path().join(&note.relative_path), changed).unwrap();
        assert!(matches!(
            editor.reconcile_external(),
            Err(AppError::IdentityChanged)
        ));
        assert_eq!(editor.snapshot(), original);
    }

    #[test]
    fn dirty_external_conflict_preserves_both_sides_and_checks_hash() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "base").unwrap();
        let mut session = connect_session(&notes, &state);
        session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: howler_editor::TextRange::new(4, 4),
                text: " local".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        fs::write(notes.path().join("note.md"), "external").unwrap();
        let reconciled = session.reconcile_active();
        let conflict = reconciled
            .state
            .active
            .as_ref()
            .unwrap()
            .conflict
            .clone()
            .unwrap();
        assert_eq!(conflict.external_source, "external");
        assert_eq!(
            reconciled
                .state
                .active
                .as_ref()
                .map(|active| active.editor.snapshot.source.as_str()),
            Some("base local")
        );
        let rejected = session.resolve_conflict(ConflictResolution::UseExternal {
            expected_external_hash: "stale".into(),
        });
        assert!(matches!(
            rejected.outcome,
            OperationOutcome::Rejected(ApplicationProblem {
                code: ProblemCode::ContentHashMismatch,
                ..
            })
        ));
        let resolved = session.resolve_conflict(ConflictResolution::UseExternal {
            expected_external_hash: conflict.external_hash,
        });
        assert!(matches!(resolved.outcome, OperationOutcome::Applied(_)));
        assert_eq!(
            resolved.state.active.unwrap().editor.snapshot.source,
            "external"
        );
    }

    #[test]
    fn conflict_keep_local_retry_is_idempotent_after_cleanup_failure() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "base").unwrap();
        let mut session = connect_session(&notes, &state);
        session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: TextRange::new(4, 4),
                text: " local".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        fs::write(notes.path().join("note.md"), "external").unwrap();
        let conflict = session
            .reconcile_active()
            .state
            .active
            .unwrap()
            .conflict
            .unwrap();
        let recovery_path = session
            .active
            .as_ref()
            .unwrap()
            .editor
            .recovery_path
            .clone();
        fs::remove_file(&recovery_path).unwrap();
        fs::create_dir(&recovery_path).unwrap();
        let resolution = ConflictResolution::KeepLocalAsNewNote {
            operation_id: "conflict-copy-1".into(),
            expected_external_hash: conflict.external_hash,
            title: Some("Local copy".into()),
        };
        let first = session.resolve_conflict(resolution.clone());
        assert!(matches!(first.outcome, OperationOutcome::Rejected(_)));
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);
        assert_eq!(
            first.state.active.unwrap().editor.snapshot.source,
            "base local"
        );

        fs::remove_dir(&recovery_path).unwrap();
        let second = session.resolve_conflict(resolution);
        assert!(matches!(second.outcome, OperationOutcome::Applied(_)));
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);
        assert_eq!(
            second.state.active.unwrap().editor.snapshot.source,
            "external"
        );
    }

    #[test]
    fn conflict_keep_local_post_success_retry_returns_same_note() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "base").unwrap();
        let mut session = connect_session(&notes, &state);
        session.apply_text_edit(HostTextEdit {
            expected_revision: 0,
            replacements: vec![howler_editor::Replacement {
                range: TextRange::new(4, 4),
                text: " local".into(),
            }],
            selections: Vec::new(),
            history: howler_editor::HistoryHint::Typing,
            composition: None,
            input_origin: None,
        });
        fs::write(notes.path().join("note.md"), "external").unwrap();
        let conflict = session
            .reconcile_active()
            .state
            .active
            .unwrap()
            .conflict
            .unwrap();
        let resolution = ConflictResolution::KeepLocalAsNewNote {
            operation_id: "conflict-post-success".into(),
            expected_external_hash: conflict.external_hash,
            title: Some("Local copy".into()),
        };
        let first = session.resolve_conflict(resolution.clone());
        let OperationOutcome::Applied(first) = first.outcome else {
            panic!("first conflict resolution was rejected");
        };
        let second_response = session.resolve_conflict(resolution);
        let active_source = second_response
            .state
            .active
            .as_ref()
            .unwrap()
            .editor
            .snapshot
            .source
            .clone();
        let OperationOutcome::Applied(second) = second_response.outcome else {
            panic!("post-success conflict retry was rejected");
        };
        assert_eq!(second.note.id, first.note.id);
        assert_eq!(session.folder_ref().unwrap().discover().unwrap().len(), 2);
        assert_eq!(active_source, "external");
    }
}
