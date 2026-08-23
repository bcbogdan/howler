#![forbid(unsafe_code)]

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use thiserror::Error;

pub const EDITOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRange {
    pub start: usize,
    pub end: usize,
}

impl TextRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Affinity {
    Upstream,
    Downstream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
    pub affinity: Affinity,
    pub revision: u64,
}

impl Selection {
    pub fn caret(offset: usize, revision: u64) -> Self {
        Self {
            anchor: offset,
            head: offset,
            affinity: Affinity::Downstream,
            revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replacement {
    pub range: TextRange,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryHint {
    Typing,
    Paste,
    Formatting,
    Isolated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub expected_revision: u64,
    pub replacements: Vec<Replacement>,
    pub selections: Vec<Selection>,
    pub history: HistoryHint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecorationKind {
    Heading(u8),
    Emphasis,
    Strong,
    Link,
    Code,
    ListItem,
    Checkbox(bool),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decoration {
    pub range: TextRange,
    pub kind: DecorationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub source: String,
    pub selections: Vec<Selection>,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditResult {
    pub revision: u64,
    pub changed_ranges: Vec<TextRange>,
    pub selections: Vec<Selection>,
    pub decorations: Vec<Decoration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorCommand {
    Bold { range: TextRange },
    Emphasis { range: TextRange },
    Link { range: TextRange, url: String },
    UnorderedList { range: TextRange },
    Checkbox { range: TextRange },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EditorError {
    #[error("expected revision {expected}, current revision is {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("range {start}..{end} is invalid for a {len}-byte document")]
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    #[error("replacement ranges overlap")]
    OverlappingEdits,
    #[error("selection is invalid")]
    InvalidSelection,
    #[error("editor command arguments are invalid")]
    InvalidCommand,
}

#[derive(Clone)]
struct AppliedTransaction {
    forward: Vec<Replacement>,
    inverse: Vec<Replacement>,
    selections_before: Vec<Selection>,
    selections_after: Vec<Selection>,
}

#[derive(Clone)]
struct HistoryGroup {
    hint: HistoryHint,
    transactions: Vec<AppliedTransaction>,
}

pub struct EditorSession {
    source: Rope,
    revision: u64,
    selections: Vec<Selection>,
    undo: Vec<HistoryGroup>,
    redo: Vec<HistoryGroup>,
}

impl EditorSession {
    pub fn new(source: &str) -> Self {
        Self {
            source: Rope::from_str(source),
            revision: 0,
            selections: vec![Selection::caret(0, 0)],
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            source: self.source.to_string(),
            selections: self.selections.clone(),
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    pub fn apply(&mut self, transaction: Transaction) -> Result<EditResult, EditorError> {
        if transaction.expected_revision != self.revision {
            return Err(EditorError::StaleRevision {
                expected: transaction.expected_revision,
                actual: self.revision,
            });
        }
        self.validate_replacements(&transaction.replacements)?;
        self.validate_post_edit_selections(&transaction)?;
        let before = self.selections.clone();
        let inverse = self.inverse_replacements(&transaction.replacements);
        let changed = changed_ranges(&transaction.replacements);
        self.apply_replacements(&transaction.replacements);
        self.revision += 1;
        self.selections = if transaction.selections.is_empty() {
            transform_selections(&before, &transaction.replacements, self.revision)
        } else {
            transaction
                .selections
                .into_iter()
                .map(|mut selection| {
                    selection.revision = self.revision;
                    selection
                })
                .collect()
        };
        let record = AppliedTransaction {
            forward: transaction.replacements,
            inverse,
            selections_before: before,
            selections_after: self.selections.clone(),
        };
        let coalesce = transaction.history == HistoryHint::Typing
            && self.undo.last().is_some_and(|group| {
                group.hint == HistoryHint::Typing && typing_contiguous(group, &record)
            });
        if coalesce {
            self.undo.last_mut().unwrap().transactions.push(record);
        } else {
            self.undo.push(HistoryGroup {
                hint: transaction.history,
                transactions: vec![record],
            });
        }
        self.redo.clear();
        Ok(self.result(changed))
    }

    pub fn execute_command(
        &mut self,
        expected_revision: u64,
        command: EditorCommand,
    ) -> Result<EditResult, EditorError> {
        if expected_revision != self.revision {
            return Err(EditorError::StaleRevision {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        let (range, replacement) = match command {
            EditorCommand::Bold { range } => (range, self.wrap(range, "**", "**")?),
            EditorCommand::Emphasis { range } => (range, self.wrap(range, "*", "*")?),
            EditorCommand::Link { range, url } => {
                if url.contains(['\r', '\n', ')']) {
                    return Err(EditorError::InvalidCommand);
                }
                (range, self.wrap(range, "[", &format!("]({url})"))?)
            }
            EditorCommand::UnorderedList { range } => {
                validate_range(&self.source, range)?;
                let text = byte_slice(&self.source, range).to_string();
                let replacement = text
                    .split_inclusive('\n')
                    .map(|line| format!("- {line}"))
                    .collect();
                (range, replacement)
            }
            EditorCommand::Checkbox { range } => {
                validate_range(&self.source, range)?;
                let text = byte_slice(&self.source, range).to_string();
                (range, format!("- [ ] {text}"))
            }
        };
        self.apply(Transaction {
            expected_revision,
            replacements: vec![Replacement {
                range,
                text: replacement,
            }],
            selections: Vec::new(),
            history: HistoryHint::Formatting,
        })
    }

    pub fn undo(&mut self, expected_revision: u64) -> Result<Option<EditResult>, EditorError> {
        self.check_revision(expected_revision)?;
        Ok(self.undo_inner())
    }

    fn undo_inner(&mut self) -> Option<EditResult> {
        let group = self.undo.pop()?;
        let selection = group.transactions.first()?.selections_before.clone();
        let mut changed = Vec::new();
        for transaction in group.transactions.iter().rev() {
            changed.extend(changed_ranges(&transaction.inverse));
            self.apply_replacements(&transaction.inverse);
        }
        self.revision += 1;
        self.selections = with_revision(selection, self.revision);
        self.redo.push(group);
        Some(self.result(changed))
    }

    pub fn redo(&mut self, expected_revision: u64) -> Result<Option<EditResult>, EditorError> {
        self.check_revision(expected_revision)?;
        Ok(self.redo_inner())
    }

    fn redo_inner(&mut self) -> Option<EditResult> {
        let group = self.redo.pop()?;
        let selection = group.transactions.last()?.selections_after.clone();
        let mut changed = Vec::new();
        for transaction in &group.transactions {
            changed.extend(changed_ranges(&transaction.forward));
            self.apply_replacements(&transaction.forward);
        }
        self.revision += 1;
        self.selections = with_revision(selection, self.revision);
        self.undo.push(group);
        Some(self.result(changed))
    }

    pub fn decorations(
        &self,
        range: TextRange,
        expected_revision: u64,
    ) -> Result<Vec<Decoration>, EditorError> {
        if expected_revision != self.revision {
            return Err(EditorError::StaleRevision {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        validate_range(&self.source, range)?;
        Ok(markdown_projection(&self.source.to_string())
            .0
            .into_iter()
            .filter(|item| overlaps(item.range, range))
            .collect())
    }

    pub fn plain_text(&self) -> String {
        markdown_projection(&self.source.to_string()).1
    }

    pub fn replace_external(&mut self, source: &str) {
        self.source = Rope::from_str(source);
        self.revision += 1;
        self.selections = vec![Selection::caret(0, self.revision)];
        self.undo.clear();
        self.redo.clear();
    }

    fn check_revision(&self, expected_revision: u64) -> Result<(), EditorError> {
        if expected_revision != self.revision {
            return Err(EditorError::StaleRevision {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        Ok(())
    }

    fn result(&self, changed_ranges: Vec<TextRange>) -> EditResult {
        EditResult {
            revision: self.revision,
            changed_ranges,
            selections: self.selections.clone(),
            decorations: markdown_projection(&self.source.to_string()).0,
        }
    }

    fn validate_replacements(&self, replacements: &[Replacement]) -> Result<(), EditorError> {
        let mut ranges: Vec<_> = replacements
            .iter()
            .map(|replacement| replacement.range)
            .collect();
        ranges.sort_by_key(|range| (range.start, range.end));
        for range in &ranges {
            validate_range(&self.source, *range)?;
        }
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(EditorError::OverlappingEdits);
        }
        Ok(())
    }

    fn validate_post_edit_selections(&self, transaction: &Transaction) -> Result<(), EditorError> {
        if transaction.selections.is_empty() {
            return Ok(());
        }
        let mut resulting = self.source.clone();
        apply_replacements_to(&mut resulting, &transaction.replacements);
        let len = resulting.len_bytes();
        if transaction.selections.iter().any(|selection| {
            selection.revision != self.revision + 1
                || selection.anchor > len
                || selection.head > len
                || !is_boundary(&resulting, selection.anchor)
                || !is_boundary(&resulting, selection.head)
        }) {
            return Err(EditorError::InvalidSelection);
        }
        Ok(())
    }

    fn wrap(&self, range: TextRange, prefix: &str, suffix: &str) -> Result<String, EditorError> {
        validate_range(&self.source, range)?;
        Ok(format!(
            "{prefix}{}{suffix}",
            byte_slice(&self.source, range)
        ))
    }

    fn inverse_replacements(&self, replacements: &[Replacement]) -> Vec<Replacement> {
        let mut delta: isize = 0;
        let mut ordered: Vec<_> = replacements.iter().collect();
        ordered.sort_by_key(|replacement| replacement.range.start);
        ordered
            .into_iter()
            .map(|replacement| {
                let old = byte_slice(&self.source, replacement.range).to_string();
                let start = (replacement.range.start as isize + delta) as usize;
                delta += replacement.text.len() as isize
                    - (replacement.range.end - replacement.range.start) as isize;
                Replacement {
                    range: TextRange::new(start, start + replacement.text.len()),
                    text: old,
                }
            })
            .collect()
    }

    fn apply_replacements(&mut self, replacements: &[Replacement]) {
        apply_replacements_to(&mut self.source, replacements);
    }
}

fn apply_replacements_to(source: &mut Rope, replacements: &[Replacement]) {
    let mut ordered: Vec<_> = replacements.iter().collect();
    ordered.sort_by_key(|replacement| std::cmp::Reverse(replacement.range.start));
    for replacement in ordered {
        let chars: Range<usize> = source.byte_to_char(replacement.range.start)
            ..source.byte_to_char(replacement.range.end);
        source.remove(chars.clone());
        source.insert(chars.start, &replacement.text);
    }
}

fn typing_contiguous(group: &HistoryGroup, current: &AppliedTransaction) -> bool {
    let Some(previous) = group.transactions.last() else {
        return false;
    };
    let ([previous], [current]) = (previous.forward.as_slice(), current.forward.as_slice()) else {
        return false;
    };
    previous.range.start == previous.range.end
        && current.range.start == current.range.end
        && current.range.start == previous.range.start + previous.text.len()
}

fn validate_range(source: &Rope, range: TextRange) -> Result<(), EditorError> {
    let len = source.len_bytes();
    if range.start > range.end
        || range.end > len
        || !is_boundary(source, range.start)
        || !is_boundary(source, range.end)
    {
        return Err(EditorError::InvalidRange {
            start: range.start,
            end: range.end,
            len,
        });
    }
    Ok(())
}

fn is_boundary(source: &Rope, byte: usize) -> bool {
    byte == source.len_bytes()
        || source.byte_to_char(byte) < source.len_chars()
            && source.char_to_byte(source.byte_to_char(byte)) == byte
}

fn byte_slice(source: &Rope, range: TextRange) -> ropey::RopeSlice<'_> {
    source.slice(source.byte_to_char(range.start)..source.byte_to_char(range.end))
}

fn changed_ranges(replacements: &[Replacement]) -> Vec<TextRange> {
    let mut delta = 0isize;
    let mut ordered: Vec<_> = replacements.iter().collect();
    ordered.sort_by_key(|replacement| replacement.range.start);
    ordered
        .into_iter()
        .map(|replacement| {
            let start = (replacement.range.start as isize + delta) as usize;
            delta += replacement.text.len() as isize
                - (replacement.range.end - replacement.range.start) as isize;
            TextRange::new(start, start + replacement.text.len())
        })
        .collect()
}

fn transform_selections(
    selections: &[Selection],
    replacements: &[Replacement],
    revision: u64,
) -> Vec<Selection> {
    selections
        .iter()
        .map(|selection| Selection {
            anchor: transform_offset(selection.anchor, replacements, selection.affinity),
            head: transform_offset(selection.head, replacements, selection.affinity),
            affinity: selection.affinity,
            revision,
        })
        .collect()
}

fn transform_offset(offset: usize, replacements: &[Replacement], affinity: Affinity) -> usize {
    let mut delta = 0isize;
    let mut ordered: Vec<_> = replacements.iter().collect();
    ordered.sort_by_key(|replacement| replacement.range.start);
    for replacement in ordered {
        let old_len = replacement.range.end - replacement.range.start;
        if offset < replacement.range.start {
            break;
        }
        if offset > replacement.range.end || offset == replacement.range.end && old_len > 0 {
            delta += replacement.text.len() as isize - old_len as isize;
        } else {
            return (replacement.range.start as isize
                + delta
                + if affinity == Affinity::Downstream {
                    replacement.text.len() as isize
                } else {
                    0
                }) as usize;
        }
    }
    (offset as isize + delta).max(0) as usize
}

fn with_revision(mut selections: Vec<Selection>, revision: u64) -> Vec<Selection> {
    for selection in &mut selections {
        selection.revision = revision;
    }
    selections
}

fn overlaps(left: TextRange, right: TextRange) -> bool {
    left.start < right.end && right.start < left.end
}

pub fn markdown_projection(source: &str) -> (Vec<Decoration>, String) {
    let body_start = front_matter_end(source).unwrap_or(0);
    let body = &source[body_start..];
    let parser = Parser::new_ext(
        body,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    )
    .into_offset_iter();
    let mut decorations = Vec::new();
    let mut plain = String::new();
    for (event, range) in parser {
        let range = TextRange::new(range.start + body_start, range.end + body_start);
        let kind = match event {
            Event::Start(Tag::Heading { level, .. }) => Some(DecorationKind::Heading(level as u8)),
            Event::Start(Tag::Emphasis) => Some(DecorationKind::Emphasis),
            Event::Start(Tag::Strong) => Some(DecorationKind::Strong),
            Event::Start(Tag::Link { .. }) => Some(DecorationKind::Link),
            Event::Start(Tag::CodeBlock(_)) | Event::Code(_) => Some(DecorationKind::Code),
            Event::Start(Tag::Item) => Some(DecorationKind::ListItem),
            Event::TaskListMarker(checked) => Some(DecorationKind::Checkbox(checked)),
            _ => None,
        };
        if let Some(kind) = kind {
            decorations.push(Decoration { range, kind });
        }
        match event {
            Event::Text(text) | Event::Code(text) => plain.push_str(&text),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph)
            | Event::End(TagEnd::Heading(_)) => plain.push('\n'),
            _ => {}
        }
    }
    (decorations, plain.trim().to_owned())
}

pub fn front_matter_end(source: &str) -> Option<usize> {
    if !source.starts_with("---\n") && !source.starts_with("---\r\n") {
        return None;
    }
    let mut offset = source.find('\n')? + 1;
    for line in source[offset..].split_inclusive('\n') {
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(offset);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(
        revision: u64,
        start: usize,
        end: usize,
        text: &str,
        history: HistoryHint,
    ) -> Transaction {
        Transaction {
            expected_revision: revision,
            replacements: vec![Replacement {
                range: TextRange::new(start, end),
                text: text.into(),
            }],
            selections: vec![],
            history,
        }
    }

    #[test]
    fn utf8_edits_require_boundaries_and_stale_edits_do_not_mutate() {
        let mut editor = EditorSession::new("a😀b");
        assert!(matches!(
            editor.apply(edit(0, 2, 2, "x", HistoryHint::Typing)),
            Err(EditorError::InvalidRange { .. })
        ));
        editor
            .apply(edit(0, 1, 5, "🙂", HistoryHint::Typing))
            .unwrap();
        let snapshot = editor.snapshot();
        assert_eq!(snapshot.source, "a🙂b");
        assert!(matches!(
            editor.apply(edit(0, 0, 0, "x", HistoryHint::Typing)),
            Err(EditorError::StaleRevision { .. })
        ));
        assert_eq!(editor.snapshot(), snapshot);
    }

    #[test]
    fn multi_edits_undo_redo_deterministically() {
        let mut editor = EditorSession::new("one two three");
        editor
            .apply(Transaction {
                expected_revision: 0,
                replacements: vec![
                    Replacement {
                        range: TextRange::new(0, 3),
                        text: "1".into(),
                    },
                    Replacement {
                        range: TextRange::new(8, 13),
                        text: "3".into(),
                    },
                ],
                selections: vec![],
                history: HistoryHint::Paste,
            })
            .unwrap();
        assert_eq!(editor.snapshot().source, "1 two 3");
        assert_eq!(editor.undo(1).unwrap().unwrap().revision, 2);
        assert_eq!(editor.snapshot().source, "one two three");
        editor.redo(2).unwrap().unwrap();
        assert_eq!(editor.snapshot().source, "1 two 3");
    }

    #[test]
    fn typing_coalesces_and_source_is_preserved() {
        let source = "---\ncustom: yes\n---\n# Title\n\n<widget x='1'>unknown</widget>\n";
        let mut editor = EditorSession::new(source);
        let end = source.len();
        editor
            .apply(edit(0, end, end, "a", HistoryHint::Typing))
            .unwrap();
        editor
            .apply(edit(1, end + 1, end + 1, "b", HistoryHint::Typing))
            .unwrap();
        editor.undo(2).unwrap().unwrap();
        assert_eq!(editor.snapshot().source, source);
        assert_eq!(editor.plain_text(), "Title\nunknown");
    }

    #[test]
    fn rejects_overlap() {
        let mut editor = EditorSession::new("abcd");
        let result = editor.apply(Transaction {
            expected_revision: 0,
            replacements: vec![
                Replacement {
                    range: TextRange::new(0, 2),
                    text: "x".into(),
                },
                Replacement {
                    range: TextRange::new(1, 3),
                    text: "y".into(),
                },
            ],
            selections: vec![],
            history: HistoryHint::Paste,
        });
        assert_eq!(result.unwrap_err(), EditorError::OverlappingEdits);
    }

    #[test]
    fn supplied_selections_are_post_edit_coordinates() {
        let mut editor = EditorSession::new("ab");
        let result = editor
            .apply(Transaction {
                expected_revision: 0,
                replacements: vec![Replacement {
                    range: TextRange::new(1, 1),
                    text: "😀".into(),
                }],
                selections: vec![Selection::caret(5, 1)],
                history: HistoryHint::Typing,
            })
            .unwrap();
        assert_eq!(result.selections, vec![Selection::caret(5, 1)]);
        assert_eq!(editor.snapshot().source, "a😀b");

        let invalid = editor.apply(Transaction {
            expected_revision: 1,
            replacements: vec![],
            selections: vec![Selection::caret(2, 2)],
            history: HistoryHint::Isolated,
        });
        assert_eq!(invalid.unwrap_err(), EditorError::InvalidSelection);
    }

    #[test]
    fn unrelated_typing_has_separate_undo_groups() {
        let mut editor = EditorSession::new("ab");
        editor
            .apply(edit(0, 1, 1, "x", HistoryHint::Typing))
            .unwrap();
        editor
            .apply(edit(1, 0, 0, "y", HistoryHint::Typing))
            .unwrap();
        editor.undo(2).unwrap().unwrap();
        assert_eq!(editor.snapshot().source, "axb");
        editor.undo(3).unwrap().unwrap();
        assert_eq!(editor.snapshot().source, "ab");
    }

    #[test]
    fn formatting_commands_preserve_unrelated_source() {
        let mut editor = EditorSession::new("before target after");
        editor
            .execute_command(
                0,
                EditorCommand::Bold {
                    range: TextRange::new(7, 13),
                },
            )
            .unwrap();
        assert_eq!(editor.snapshot().source, "before **target** after");
        editor
            .execute_command(
                1,
                EditorCommand::Link {
                    range: TextRange::new(9, 15),
                    url: "https://example.com".into(),
                },
            )
            .unwrap();
        assert_eq!(
            editor.snapshot().source,
            "before **[target](https://example.com)** after"
        );
    }
}
