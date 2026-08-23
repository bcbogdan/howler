#![forbid(unsafe_code)]

use howler_editor::{
    front_matter_end, EditResult, EditorCommand, EditorError, EditorSession, Transaction,
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

pub struct NoteFolder {
    root: PathBuf,
    state_dir: PathBuf,
    index: Connection,
    state: Connection,
    adopted: bool,
    context_id: String,
    operation_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    generations: Arc<Mutex<HashMap<String, u64>>>,
}

pub struct NoteEditor {
    editor: EditorSession,
    note_id: Identity,
    relative_path: PathBuf,
    absolute_path: PathBuf,
    recovery_path: PathBuf,
    base_hash: String,
    dirty: bool,
    owner_context_id: String,
    generation: u64,
    generations: Arc<Mutex<HashMap<String, u64>>>,
}

type GenerationRegistry = Mutex<HashMap<PathBuf, Weak<Mutex<HashMap<String, u64>>>>>;
static GENERATION_REGISTRY: OnceLock<GenerationRegistry> = OnceLock::new();

impl NoteFolder {
    pub fn create(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
    ) -> Result<Self, AppError> {
        fs::create_dir_all(root.as_ref())?;
        Self::open(root, application_state_root, adopt)
    }

    pub fn open(
        root: impl AsRef<Path>,
        application_state_root: impl AsRef<Path>,
        adopt: bool,
    ) -> Result<Self, AppError> {
        let root = fs::canonicalize(root.as_ref())?;
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
                apply_adoption_manifest(&root, &manifest)?;
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
                apply_adoption_manifest(&root, &manifest)?;
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
        let index = Connection::open(state_dir.join("index.sqlite3"))?;
        initialize_index(&index)?;
        let state = Connection::open(state_dir.join("state.sqlite3"))?;
        initialize_state(&state)?;
        let generations = folder_generations(&root);
        let folder = Self {
            root,
            state_dir,
            index,
            state,
            adopted: library_id.is_some(),
            context_id: Ulid::new().to_string(),
            operation_locks: Mutex::new(HashMap::new()),
            generations,
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
            adopt_source(initial_source.unwrap_or(""), &id)
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

    pub fn open_editor(&self, id: &str) -> Result<NoteEditor, AppError> {
        if self.recoveries()?.iter().any(|draft| draft.note_id == id) {
            return Err(AppError::RecoveryPending(id.into()));
        }
        let note = self.find_unique_note(id)?;
        self.editor_for_note(note, None)
    }

    pub fn restore_recovery(&self, id: &str) -> Result<NoteEditor, AppError> {
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
        let actual = editor.snapshot().revision;
        if expected_revision != actual {
            return Err(EditorError::StaleRevision {
                expected: expected_revision,
                actual,
            }
            .into());
        }
        let lock = self.note_lock(editor.note_id.as_str());
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let mut editor = self.open_editor(id)?;
        let source = editor.editor.snapshot().source;
        let replacement = rename_title_source(&source, title)?;
        editor.editor.replace_external(&replacement);
        editor.dirty = true;
        let _ = editor.persist_recovery();
        let revision = editor.snapshot().revision;
        self.save_editor(&mut editor, revision)?;
        self.summary_for_path(&editor.relative_path)
    }

    pub fn move_note(
        &self,
        id: &str,
        destination: impl AsRef<Path>,
    ) -> Result<NoteSummary, AppError> {
        let note = self.find_unique_note(id)?;
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_pending_recovery(id)?;
        let from = self.resolve_existing(&note.relative_path)?;
        let destination = normalized_relative(destination.as_ref())?;
        if !is_markdown(&destination) {
            return Err(AppError::PathEscape);
        }
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
        self.bump_generation(id);
        let summary = self.summary_for_path(&destination)?;
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
        Ok(summary)
    }

    pub fn trash(&self, id: &str) -> Result<PathBuf, AppError> {
        let note = self.find_unique_note(id)?;
        let lock = self.note_lock(id);
        let _operation = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        Ok(NoteEditor {
            editor,
            recovery_path: self.recovery_path(note.id.as_str()),
            note_id: note.id,
            relative_path: note.relative_path,
            absolute_path,
            base_hash,
            dirty,
            owner_context_id: self.context_id.clone(),
            generation,
            generations: Arc::clone(&self.generations),
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

    fn recovery_path(&self, id: &str) -> PathBuf {
        self.state_dir
            .join("recovery")
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

    pub fn apply(&mut self, transaction: Transaction) -> Result<MutationOutcome, AppError> {
        self.ensure_current_generation()?;
        let result = self.editor.apply(transaction)?;
        self.accepted(Some(result))
    }

    pub fn execute_command(
        &mut self,
        expected_revision: u64,
        command: EditorCommand,
    ) -> Result<MutationOutcome, AppError> {
        self.ensure_current_generation()?;
        let result = self.editor.execute_command(expected_revision, command)?;
        self.accepted(Some(result))
    }

    pub fn undo(&mut self, expected_revision: u64) -> Result<MutationOutcome, AppError> {
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
        self.ensure_current_generation()?;
        let external = read_utf8(&self.absolute_path, &self.relative_path)?;
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

fn folder_generations(root: &Path) -> Arc<Mutex<HashMap<String, u64>>> {
    let registry = GENERATION_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(generations) = registry.get(root).and_then(Weak::upgrade) {
        return generations;
    }
    let generations = Arc::new(Mutex::new(HashMap::new()));
    registry.insert(root.to_path_buf(), Arc::downgrade(&generations));
    generations
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

fn apply_adoption_manifest(root: &Path, manifest: &AdoptionManifest) -> Result<(), AppError> {
    let generations = folder_generations(root);
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
            if source.file_name().and_then(|value| value.to_str()) == Some("recovery") {
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
        assert_eq!(folder.open_editor(&id).unwrap().note_id().as_str(), id);
        assert_eq!(folder.search("body", 10).unwrap()[0].note.id.as_str(), id);
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
        apply_adoption_manifest(&root, &manifest).unwrap();
        apply_adoption_manifest(&root, &manifest).unwrap();
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
        apply_adoption_manifest(folder.root(), &manifest).unwrap();
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
}
