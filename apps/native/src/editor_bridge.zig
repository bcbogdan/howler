const std = @import("std");
const native_sdk = @import("native_sdk");
const session_types = @import("session_types.zig");

const canvas = native_sdk.canvas;

pub const max_source_bytes: usize = 1024 * 1024;
const SourceBuffer = canvas.TextBuffer(max_source_bytes);

pub const Action = union(enum) {
    none,
    selection: SelectionRequest,
    edit: TextEditRequest,
    undo: u64,
    redo: u64,
};

pub const SelectionRequest = struct {
    expected_revision: u64,
    selections: []const session_types.Selection,
};

pub const TextRange = struct {
    start: usize,
    end: usize,
};

pub const Replacement = struct {
    range: TextRange,
    text: []const u8,
};

pub const HistoryHint = enum { Typing, Paste, Formatting, Isolated };
pub const InputOrigin = enum { typing, paste, composition, dictation, autocorrection, replacement };

pub const CompositionCommit = struct {
    original_range: TextRange,
    original_text: []const u8,
};

pub const TextEditRequest = struct {
    expected_revision: u64,
    replacements: []const Replacement,
    selections: []const session_types.Selection,
    history: HistoryHint,
    composition: ?CompositionCommit,
    input_origin: ?InputOrigin,
};

pub const Bridge = struct {
    authoritative: SourceBuffer = .{},
    mirror: SourceBuffer = .{},
    candidate: SourceBuffer = .{},
    revision: u64 = 0,
    selection_storage: [1]session_types.Selection = undefined,
    replacement_storage: [1]Replacement = undefined,
    pending_committed_input: bool = false,
    oversized: bool = false,

    pub fn install(bridge: *Bridge, snapshot: session_types.Snapshot) void {
        bridge.revision = snapshot.revision;
        bridge.oversized = snapshot.source.len > max_source_bytes;
        if (bridge.oversized) {
            bridge.authoritative.clear();
            bridge.mirror.clear();
            bridge.pending_committed_input = false;
            return;
        }
        bridge.authoritative.set(snapshot.source);
        bridge.mirror.set(snapshot.source);
        bridge.installSelection(snapshot);
        bridge.pending_committed_input = false;
    }

    pub fn installAuthoritativePreservingMirror(bridge: *Bridge, snapshot: session_types.Snapshot) void {
        bridge.revision = snapshot.revision;
        bridge.oversized = snapshot.source.len > max_source_bytes;
        if (bridge.oversized) {
            bridge.authoritative.clear();
        } else {
            bridge.authoritative.set(snapshot.source);
        }
        bridge.pending_committed_input = true;
    }

    pub fn presentPendingDraft(bridge: *Bridge, snapshot: session_types.Snapshot, source_text: []const u8) void {
        bridge.revision = snapshot.revision;
        bridge.oversized = snapshot.source.len > max_source_bytes or source_text.len > max_source_bytes;
        if (bridge.oversized) {
            bridge.authoritative.clear();
            bridge.mirror.clear();
            bridge.pending_committed_input = false;
            return;
        }
        bridge.authoritative.set(snapshot.source);
        bridge.mirror.set(source_text);
        bridge.pending_committed_input = false;
    }

    pub fn source(bridge: *const Bridge) []const u8 {
        return bridge.mirror.text();
    }

    pub fn selection(bridge: *const Bridge) canvas.TextSelection {
        return bridge.mirror.selection;
    }

    pub fn composing(bridge: *const Bridge) bool {
        return bridge.mirror.composition != null;
    }

    pub fn applyEditorEvent(bridge: *Bridge, event: canvas.EditorEvent) Action {
        if (bridge.oversized or event.revision != bridge.revision or bridge.pending_committed_input) return .none;
        switch (event.command) {
            .undo => return .{ .undo = bridge.revision },
            .redo => return .{ .redo = bridge.revision },
            .none => {},
        }

        bridge.selection_storage[0] = selectionFromEvent(event.selection, bridge.revision);
        const edit = event.edit orelse return .{ .selection = .{
            .expected_revision = bridge.revision,
            .selections = bridge.selection_storage[0..1],
        } };

        bridge.candidate = bridge.mirror;
        bridge.candidate.apply(edit);
        if (bridge.candidate.truncated) return .none;
        bridge.mirror = bridge.candidate;
        switch (edit) {
            .set_composition, .cancel_composition => return .none,
            else => {},
        }

        const changed = replacementBetween(bridge.authoritative.text(), bridge.mirror.text());
        bridge.replacement_storage[0] = changed;
        bridge.selection_storage[0] = selectionFromEvent(event.selection, bridge.revision + 1);
        bridge.pending_committed_input = true;
        const origin = originFromEvent(event.origin);
        return .{ .edit = .{
            .expected_revision = bridge.revision,
            .replacements = bridge.replacement_storage[0..1],
            .selections = bridge.selection_storage[0..1],
            .history = historyFromOrigin(origin),
            .composition = if (origin == .composition) .{
                .original_range = changed.range,
                .original_text = bridge.authoritative.text()[changed.range.start..changed.range.end],
            } else null,
            .input_origin = origin,
        } };
    }

    pub fn clearPending(bridge: *Bridge) void {
        bridge.pending_committed_input = false;
    }

    fn installSelection(bridge: *Bridge, snapshot: session_types.Snapshot) void {
        if (snapshot.selections.len == 0) return;
        const snapshot_selection = snapshot.selections[0];
        bridge.mirror.selection = .{
            .anchor = snapshot_selection.anchor,
            .focus = snapshot_selection.head,
            .affinity = switch (snapshot_selection.affinity) {
                .Upstream => .upstream,
                .Downstream => .downstream,
            },
        };
    }
};

pub fn encodeJson(allocator: std.mem.Allocator, value: anytype) ![]u8 {
    var output: std.Io.Writer.Allocating = .init(allocator);
    errdefer output.deinit();
    var stringify: std.json.Stringify = .{ .writer = &output.writer };
    try stringify.write(value);
    return output.toOwnedSlice();
}

fn selectionFromEvent(selection: canvas.TextSelection, revision: u64) session_types.Selection {
    return .{
        .anchor = selection.anchor,
        .head = selection.focus,
        .affinity = switch (selection.affinity) {
            .upstream => .Upstream,
            .downstream => .Downstream,
        },
        .revision = revision,
    };
}

fn originFromEvent(origin: canvas.TextInputOrigin) InputOrigin {
    return switch (origin) {
        .typing => .typing,
        .paste => .paste,
        .composition => .composition,
        .dictation => .dictation,
        .autocorrection => .autocorrection,
        .replacement => .replacement,
    };
}

fn historyFromOrigin(origin: InputOrigin) HistoryHint {
    return switch (origin) {
        .typing => .Typing,
        .paste => .Paste,
        .composition, .dictation, .autocorrection, .replacement => .Isolated,
    };
}

fn replacementBetween(old: []const u8, new: []const u8) Replacement {
    var prefix: usize = 0;
    const shared = @min(old.len, new.len);
    while (prefix < shared and old[prefix] == new[prefix]) prefix += 1;
    while (prefix > 0 and prefix < old.len and (old[prefix] & 0xc0) == 0x80) prefix -= 1;
    while (prefix > 0 and prefix < new.len and (new[prefix] & 0xc0) == 0x80) prefix -= 1;

    var suffix: usize = 0;
    while (suffix < old.len - prefix and
        suffix < new.len - prefix and
        old[old.len - 1 - suffix] == new[new.len - 1 - suffix])
    {
        suffix += 1;
    }
    var old_end = old.len - suffix;
    var new_end = new.len - suffix;
    while (old_end < old.len and (old[old_end] & 0xc0) == 0x80) {
        old_end += 1;
        new_end += 1;
    }
    return .{
        .range = .{ .start = prefix, .end = old_end },
        .text = new[prefix..new_end],
    };
}

test "committed Unicode input becomes one revision-stamped replacement" {
    var bridge = Bridge{};
    bridge.install(.{
        .revision = 2,
        .source = "ab",
        .selections = &.{.{ .anchor = 1, .head = 1, .affinity = .Downstream, .revision = 2 }},
        .can_undo = false,
        .can_redo = false,
    });
    bridge.mirror.selection = canvas.TextSelection.collapsed(1);

    const action = bridge.applyEditorEvent(.{
        .edit = .{ .insert_text = "😀" },
        .origin = .typing,
        .revision = 2,
        .selection = canvas.TextSelection.collapsed(5),
    });
    try std.testing.expect(action == .edit);
    try std.testing.expectEqual(@as(usize, 1), action.edit.replacements[0].range.start);
    try std.testing.expectEqual(@as(usize, 1), action.edit.replacements[0].range.end);
    try std.testing.expectEqualStrings("😀", action.edit.replacements[0].text);
    try std.testing.expectEqual(@as(u64, 3), action.edit.selections[0].revision);
}

test "composition preedit remains local until commit" {
    var bridge = Bridge{};
    bridge.install(.{
        .revision = 1,
        .source = "a",
        .selections = &.{.{ .anchor = 1, .head = 1, .affinity = .Downstream, .revision = 1 }},
        .can_undo = false,
        .can_redo = false,
    });
    bridge.mirror.selection = canvas.TextSelection.collapsed(1);
    const preedit = bridge.applyEditorEvent(.{
        .edit = .{ .set_composition = .{ .text = "é", .cursor = 2 } },
        .origin = .composition,
        .revision = 1,
        .selection = canvas.TextSelection.collapsed(3),
    });
    try std.testing.expect(preedit == .none);
    try std.testing.expect(bridge.composing());

    const committed = bridge.applyEditorEvent(.{
        .edit = .commit_composition,
        .origin = .composition,
        .revision = 1,
        .selection = canvas.TextSelection.collapsed(3),
    });
    try std.testing.expect(committed == .edit);
    try std.testing.expectEqualStrings("é", committed.edit.replacements[0].text);
    try std.testing.expectEqualStrings("", committed.edit.composition.?.original_text);
}

test "oversized authoritative source is refused instead of truncated" {
    const source = try std.testing.allocator.alloc(u8, max_source_bytes + 1);
    defer std.testing.allocator.free(source);
    @memset(source, 'a');
    var bridge = Bridge{};
    bridge.install(.{
        .revision = 9,
        .source = source,
        .selections = &.{},
        .can_undo = false,
        .can_redo = false,
    });
    try std.testing.expect(bridge.oversized);
    try std.testing.expectEqual(@as(usize, 0), bridge.source().len);
    try std.testing.expect(bridge.applyEditorEvent(.{ .revision = 9 }) == .none);
}

test "over-capacity input leaves the controlled mirror unchanged" {
    const source = try std.testing.allocator.alloc(u8, max_source_bytes);
    defer std.testing.allocator.free(source);
    @memset(source, 'a');
    var bridge = Bridge{};
    bridge.install(.{
        .revision = 1,
        .source = source,
        .selections = &.{},
        .can_undo = false,
        .can_redo = false,
    });
    bridge.mirror.selection = canvas.TextSelection.collapsed(source.len);
    const action = bridge.applyEditorEvent(.{
        .edit = .{ .insert_text = "b" },
        .revision = 1,
        .selection = canvas.TextSelection.collapsed(source.len),
    });
    try std.testing.expect(action == .none);
    try std.testing.expectEqual(source.len, bridge.source().len);
    try std.testing.expectEqual(@as(u8, 'a'), bridge.source()[bridge.source().len - 1]);
}
