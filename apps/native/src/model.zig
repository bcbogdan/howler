const std = @import("std");
const native_sdk = @import("native_sdk");
const editor_bridge = @import("editor_bridge.zig");
const effect_registry = @import("effects.zig");
const session_mod = @import("session.zig");
const session_types = @import("session_types.zig");

const canvas = native_sdk.canvas;
const QueryBuffer = canvas.TextBuffer(256);
const max_search_results = 10;

pub const SearchRow = struct {
    index: usize = 0,
    title: []const u8 = "",
    snippet: []const u8 = "",
};

pub const Model = struct {
    session: *session_mod.Session,
    bridge: editor_bridge.Bridge = .{},
    autosaves: effect_registry.Registry = .{},
    query: QueryBuffer = .{},
    status_storage: [384]u8 = undefined,
    status_len: usize = 0,
    folder_storage: [1024]u8 = undefined,
    folder_len: usize = 0,
    capabilities_ready: bool = false,
    palette_visible: bool = false,
    pending_draft: bool = false,
    conflict_visible: bool = false,
    replacement_safe: bool = true,
    active_editor: bool = false,
    pending_edit_id: ?u64 = null,
    conflict_hash_storage: [128]u8 = undefined,
    conflict_hash_len: usize = 0,
    recovery_id_storage: [128]u8 = undefined,
    recovery_id_len: usize = 0,
    recovery_path_storage: [512]u8 = undefined,
    recovery_path_len: usize = 0,
    recovery_count: usize = 0,
    search_rows: [max_search_results]SearchRow = @splat(.{}),
    search_title_storage: [max_search_results][256]u8 = undefined,
    search_snippet_storage: [max_search_results][512]u8 = undefined,
    search_note_storage: [max_search_results][128]u8 = undefined,
    search_note_len: [max_search_results]usize = @splat(0),
    search_count: usize = 0,
    pending_search_id: ?u64 = null,
    operation_serial: u64 = 0,
    folder_picker_serial: u64 = 0,
    close_requested: bool = false,
    close_deferred: bool = false,
    close_save_in_flight: bool = false,
    close_save_failed: bool = false,

    pub const view_unbound = .{
        "session",
        "bridge",
        "autosaves",
        "query",
        "status_storage",
        "status_len",
        "folder_storage",
        "folder_len",
        "capabilities_ready",
        "folderPath",
        "editorRevision",
        "selectionAnchor",
        "selectionFocus",
        "canReplaceEditor",
        "replacement_safe",
        "folder_picker_serial",
        "close_requested",
        "close_deferred",
        "close_save_in_flight",
        "close_save_failed",
        "active_editor",
        "pending_edit_id",
        "conflict_hash_storage",
        "conflict_hash_len",
        "recovery_id_storage",
        "recovery_id_len",
        "recovery_path_storage",
        "recovery_path_len",
        "operation_serial",
        "recovery_count",
        "search_rows",
        "search_title_storage",
        "search_snippet_storage",
        "search_note_storage",
        "search_note_len",
        "search_count",
        "pending_search_id",
    };

    pub fn init(session: *session_mod.Session) Model {
        var model = Model{ .session = session };
        model.setStatus("Starting Rust application session", .{});
        return model;
    }

    pub fn status(model: *const Model) []const u8 {
        return model.status_storage[0..model.status_len];
    }

    pub fn folderPath(model: *const Model) []const u8 {
        return model.folder_storage[0..model.folder_len];
    }

    pub fn folderLabel(model: *const Model) []const u8 {
        return if (model.folder_len == 0) "No folder selected" else std.fs.path.basename(model.folderPath());
    }

    pub fn connected(model: *const Model) bool {
        return model.folder_len > 0;
    }

    pub fn hasEditor(model: *const Model) bool {
        return model.active_editor;
    }

    pub fn editorSource(model: *const Model) []const u8 {
        return model.bridge.source();
    }

    pub fn editorRevision(model: *const Model) u64 {
        return model.bridge.revision;
    }

    pub fn selectionAnchor(model: *const Model) usize {
        return model.bridge.selection().anchor;
    }

    pub fn selectionFocus(model: *const Model) usize {
        return model.bridge.selection().focus;
    }

    pub fn editorEditable(model: *const Model) bool {
        return model.capabilities_ready and model.connected() and model.hasEditor() and
            !model.bridge.pending_committed_input and !model.pending_draft and !model.bridge.oversized;
    }

    pub fn saveDisabled(model: *const Model) bool {
        return !model.editorEditable() or model.bridge.composing();
    }

    pub fn queryText(model: *const Model) []const u8 {
        return model.query.text();
    }

    pub fn searchResults(model: *const Model) []const SearchRow {
        return model.search_rows[0..model.search_count];
    }

    pub fn hasRecoveries(model: *const Model) bool {
        return model.recovery_count > 0;
    }

    pub fn recoveryLabel(model: *const Model) []const u8 {
        return model.recovery_path_storage[0..model.recovery_path_len];
    }

    pub fn setStatus(model: *Model, comptime format: []const u8, args: anytype) void {
        const text = std.fmt.bufPrint(&model.status_storage, format, args) catch {
            model.status_len = 0;
            return;
        };
        model.status_len = text.len;
    }

    pub fn requestCapabilities(model: *Model) void {
        _ = model.session.submit(.capabilities, .background, null, 0) catch {
            model.setStatus("Could not queue capability negotiation", .{});
        };
    }

    pub fn createNote(model: *Model) void {
        if (!model.connected() or model.bridge.pending_committed_input) return;
        _ = model.session.submit(.create_note, .background, "{\"source\":null}", 0) catch {
            model.setStatus("Could not queue note creation", .{});
        };
    }

    pub fn togglePalette(model: *Model) void {
        model.palette_visible = !model.palette_visible;
        if (model.palette_visible) model.requestSearch();
    }

    pub fn dismissPalette(model: *Model) void {
        model.palette_visible = false;
    }

    pub fn chooseFolder(model: *Model) void {
        if (!model.canReplaceEditor()) {
            model.setStatus("Committed input must become durable before changing folders", .{});
            return;
        }
        model.folder_picker_serial +%= 1;
        if (model.folder_picker_serial == 0) model.folder_picker_serial = 1;
    }

    pub fn connect(model: *Model, folder_path: []const u8, application_state_path: []const u8) void {
        _ = model.submitJson(.connect, .background, .{
            .folder_path = folder_path,
            .application_state_path = application_state_path,
            .adopt = false,
            .create_missing = false,
        }, 0);
        model.setStatus("Opening notes folder", .{});
    }

    pub fn requestClose(model: *Model) void {
        if (!model.canReplaceEditor()) {
            model.close_deferred = true;
            model.close_save_failed = false;
            model.close_save_in_flight = model.queueSave();
            model.setStatus("Close deferred until committed input is durable", .{});
            return;
        }
        model.close_deferred = false;
        model.close_requested = true;
    }

    pub fn discardPendingDraft(model: *Model) void {
        if (!model.pending_draft) return;
        _ = model.submitJson(.resolve_pending_native_draft, .committed_input, .{ .resolution = "discard" }, 0);
    }

    pub fn savePendingDraftAsNew(model: *Model) void {
        if (!model.pending_draft) return;
        var operation_storage: [128]u8 = undefined;
        const operation_id = model.nextOperationId(&operation_storage) orelse return;
        _ = model.submitJson(.resolve_pending_native_draft, .committed_input, .{
            .resolution = "save_as_new",
            .operation_id = operation_id,
            .title = @as(?[]const u8, null),
        }, 0);
    }

    pub fn useExternalConflict(model: *Model) void {
        if (!model.conflict_visible) return;
        _ = model.submitJson(.resolve_conflict, .committed_input, .{
            .resolution = "use_external",
            .expected_external_hash = model.conflict_hash_storage[0..model.conflict_hash_len],
        }, 0);
    }

    pub fn keepConflictAsNew(model: *Model) void {
        if (!model.conflict_visible) return;
        var operation_storage: [128]u8 = undefined;
        const operation_id = model.nextOperationId(&operation_storage) orelse return;
        _ = model.submitJson(.resolve_conflict, .committed_input, .{
            .resolution = "keep_local_as_new_note",
            .operation_id = operation_id,
            .expected_external_hash = model.conflict_hash_storage[0..model.conflict_hash_len],
            .title = @as(?[]const u8, null),
        }, 0);
    }

    pub fn restoreRecovery(model: *Model) void {
        if (model.recovery_id_len == 0 or !model.canReplaceEditor()) return;
        model.submitRaw(.restore_recovery, model.recovery_id_storage[0..model.recovery_id_len]);
    }

    pub fn discardRecovery(model: *Model) void {
        if (model.recovery_id_len == 0) return;
        model.submitRaw(.discard_recovery, model.recovery_id_storage[0..model.recovery_id_len]);
    }

    pub fn canReplaceEditor(model: *const Model) bool {
        return model.replacement_safe and !model.bridge.pending_committed_input and
            !model.bridge.composing() and !model.pending_draft;
    }

    pub fn undo(model: *Model) void {
        if (!model.editorEditable()) return;
        model.submitWithoutPayload(.undo, .committed_input, model.bridge.revision);
    }

    pub fn redo(model: *Model) void {
        if (!model.editorEditable()) return;
        model.submitWithoutPayload(.redo, .committed_input, model.bridge.revision);
    }

    pub fn editQuery(model: *Model, event: canvas.TextInputEvent) void {
        model.query.apply(event);
        model.requestSearch();
    }

    pub fn openSearchResult(model: *Model, index: usize) void {
        if (index >= model.search_count or !model.canReplaceEditor()) return;
        model.submitRaw(.open_note, model.search_note_storage[index][0..model.search_note_len[index]]);
        model.palette_visible = false;
    }

    pub fn handleEditorEvent(model: *Model, event: canvas.EditorEvent) void {
        const action = model.bridge.applyEditorEvent(event);
        switch (action) {
            .none => {},
            .selection => |request| _ = model.submitJson(.apply_selection, .committed_input, request, request.expected_revision),
            .edit => |request| model.pending_edit_id = model.submitJson(.apply_text_edit, .committed_input, request, request.expected_revision),
            .undo => |revision| model.submitWithoutPayload(.undo, .committed_input, revision),
            .redo => |revision| model.submitWithoutPayload(.redo, .committed_input, revision),
        }
        model.finishDeferredClose(.state, false);
    }

    pub fn save(model: *Model) void {
        _ = model.queueSave();
    }

    fn queueSave(model: *Model) bool {
        if (model.bridge.composing() or model.bridge.pending_committed_input) return false;
        for (&model.autosaves.entries) |*entry| {
            if (!entry.used) continue;
            return model.submitJson(.save, .background, entry.target(), 0) != null;
        }
        return false;
    }

    pub fn autosaveFired(model: *Model, key: u64) void {
        const target = model.autosaves.takeTarget(key) orelse return;
        _ = model.submitJson(.save, .background, target, 0);
    }

    pub fn processCompletions(model: *Model, fx: anytype, autosave_message: anytype) void {
        while (model.session.poll()) |completion_value| {
            var completion = completion_value;
            defer completion.deinit();
            model.processCompletion(&completion, fx, autosave_message);
        }
    }

    fn processCompletion(model: *Model, completion: *const session_mod.Completion, fx: anytype, autosave_message: anytype) void {
        if (completion.status == .busy) {
            const retry_id = model.session.submit(
                completion.operation,
                completion.priority,
                if (completion.payload) |payload| payload else null,
                completion.revision,
            ) catch {
                model.setStatus("Could not retry busy Rust operation", .{});
                return;
            };
            if (model.pending_edit_id == completion.id) model.pending_edit_id = retry_id;
            if (model.pending_search_id == completion.id) model.pending_search_id = retry_id;
            return;
        }
        if (completion.status != .ok) {
            model.clearFailedSearch(completion);
            if (completion.operation == .save) {
                model.close_save_in_flight = false;
                model.close_save_failed = true;
            }
            model.setStatus("Rust transport failed: {s}", .{@tagName(completion.status)});
            return;
        }
        const json = completion.response orelse {
            model.clearFailedSearch(completion);
            if (completion.operation == .save) {
                model.close_save_in_flight = false;
                model.close_save_failed = true;
            }
            model.setStatus("Rust returned no response", .{});
            return;
        };

        var arena_state = std.heap.ArenaAllocator.init(std.heap.page_allocator);
        defer arena_state.deinit();
        if (completion.operation == .capabilities) {
            const response = session_types.decodeResponse(session_types.SessionCapabilities, arena_state.allocator(), json) catch {
                model.setStatus("Capability response violated ABI v2", .{});
                return;
            };
            model.installState(response.state);
            switch (response.outcome) {
                .applied => |capabilities| {
                    model.capabilities_ready = capabilities.application_session_abi == 2 and
                        capabilities.selection_updates and capabilities.input_origin_metadata and
                        capabilities.rust_owned_history and capabilities.pending_native_drafts;
                    if (model.capabilities_ready) {
                        model.setStatus("Ready", .{});
                    } else {
                        model.setStatus("Required Rust capabilities are unavailable", .{});
                    }
                },
                .rejected => |problem| model.setProblem(problem),
            }
            model.finishDeferredClose(completion.operation, false);
            return;
        }
        if (completion.operation == .search) {
            if (model.pending_search_id != completion.id) return;
            model.pending_search_id = null;
            const response = session_types.decodeResponse([]const session_types.SearchResult, arena_state.allocator(), json) catch {
                model.pending_search_id = null;
                model.search_count = 0;
                model.setStatus("Search response violated ABI v2", .{});
                return;
            };
            if (!model.bridge.pending_committed_input and !model.bridge.composing()) model.installState(response.state);
            model.search_count = 0;
            switch (response.outcome) {
                .applied => |results| model.installSearchResults(results),
                .rejected => |problem| model.setProblem(problem),
            }
            model.finishDeferredClose(completion.operation, false);
            return;
        }

        const response = session_types.decodeResponse(std.json.Value, arena_state.allocator(), json) catch {
            model.setStatus("Rust response violated ABI v2", .{});
            return;
        };
        const owns_pending_edit = model.completionOwnsPendingEdit(completion);
        const rejected_edit = owns_pending_edit and response.outcome == .rejected;
        if (rejected_edit and response.state.active != null) {
            model.preservePendingDraft(completion.revision);
            model.bridge.installAuthoritativePreservingMirror(response.state.active.?.editor.snapshot);
            model.pending_edit_id = null;
        } else if (owns_pending_edit or (!model.bridge.pending_committed_input and !model.bridge.composing())) {
            model.installState(response.state);
            if (owns_pending_edit) model.pending_edit_id = null;
        }
        model.applyEffects(response.effects, fx, autosave_message);
        switch (response.outcome) {
            .applied => if (model.bridge.oversized) {
                model.setStatus("This note exceeds the 1 MiB editor limit and is read-only", .{});
            } else {
                model.setStatus("Ready", .{});
            },
            .rejected => |problem| model.setProblem(problem),
        }
        model.finishDeferredClose(completion.operation, response.outcome == .rejected);
    }

    fn completionOwnsPendingEdit(model: *const Model, completion: *const session_mod.Completion) bool {
        return completion.operation == .apply_text_edit and model.pending_edit_id == completion.id;
    }

    fn finishDeferredClose(model: *Model, operation: session_mod.Operation, rejected: bool) void {
        if (!model.close_deferred) return;
        if (operation == .save) {
            model.close_save_in_flight = false;
            model.close_save_failed = rejected;
        }
        if (model.canReplaceEditor()) {
            model.close_deferred = false;
            model.close_requested = true;
            return;
        }
        if (operation == .save) model.close_save_failed = true;
        if (!model.close_save_in_flight and !model.close_save_failed) {
            model.close_save_in_flight = model.queueSave();
        }
    }

    fn requestSearch(model: *Model) void {
        if (!model.connected()) return;
        model.pending_search_id = model.submitJson(.search, .background, .{
            .query = model.query.text(),
            .limit = max_search_results,
        }, 0);
    }

    fn clearFailedSearch(model: *Model, completion: *const session_mod.Completion) void {
        if (completion.operation != .search or model.pending_search_id != completion.id) return;
        model.pending_search_id = null;
        model.search_count = 0;
    }

    fn installSearchResults(model: *Model, results: []const session_types.SearchResult) void {
        model.search_count = 0;
        for (results[0..@min(results.len, max_search_results)]) |result| {
            if (result.note.id.value.len > model.search_note_storage[0].len) continue;
            const index = model.search_count;
            const title_len = utf8PrefixLen(result.note.title, model.search_title_storage[index].len);
            @memcpy(model.search_title_storage[index][0..title_len], result.note.title[0..title_len]);
            const snippet_len = utf8PrefixLen(result.snippet, model.search_snippet_storage[index].len);
            @memcpy(model.search_snippet_storage[index][0..snippet_len], result.snippet[0..snippet_len]);
            model.search_note_len[index] = result.note.id.value.len;
            @memcpy(model.search_note_storage[index][0..model.search_note_len[index]], result.note.id.value[0..model.search_note_len[index]]);
            model.search_rows[index] = .{
                .index = index,
                .title = model.search_title_storage[index][0..title_len],
                .snippet = model.search_snippet_storage[index][0..snippet_len],
            };
            model.search_count += 1;
        }
    }

    fn installState(model: *Model, state: session_types.ApplicationState) void {
        if (state.folder) |folder| {
            const len = @min(folder.path.len, model.folder_storage.len);
            @memcpy(model.folder_storage[0..len], folder.path[0..len]);
            model.folder_len = len;
        } else {
            model.folder_len = 0;
        }
        model.recovery_count = state.recoveries.len;
        model.recovery_id_len = 0;
        model.recovery_path_len = 0;
        if (state.recoveries.len > 0) {
            const recovery = state.recoveries[0];
            model.recovery_id_len = @min(recovery.note_id.len, model.recovery_id_storage.len);
            @memcpy(model.recovery_id_storage[0..model.recovery_id_len], recovery.note_id[0..model.recovery_id_len]);
            model.recovery_path_len = @min(recovery.relative_path.len, model.recovery_path_storage.len);
            @memcpy(model.recovery_path_storage[0..model.recovery_path_len], recovery.relative_path[0..model.recovery_path_len]);
        }
        if (state.active) |active| {
            model.active_editor = true;
            model.pending_draft = active.pending_native_draft != null;
            model.conflict_visible = active.conflict != null;
            model.conflict_hash_len = 0;
            if (active.conflict) |conflict| {
                model.conflict_hash_len = @min(conflict.external_hash.len, model.conflict_hash_storage.len);
                @memcpy(model.conflict_hash_storage[0..model.conflict_hash_len], conflict.external_hash[0..model.conflict_hash_len]);
            }
            model.replacement_safe = active.persistence.replacement_safety == .safe;
            if (active.pending_native_draft) |draft| {
                model.bridge.presentPendingDraft(active.editor.snapshot, draft.source);
            } else {
                model.bridge.install(active.editor.snapshot);
            }
        } else {
            model.active_editor = false;
            model.bridge.install(.{ .revision = 0, .source = "", .selections = &.{}, .can_undo = false, .can_redo = false });
            model.pending_draft = false;
            model.conflict_visible = false;
            model.conflict_hash_len = 0;
            model.replacement_safe = true;
        }
    }

    fn preservePendingDraft(model: *Model, base_revision: u64) void {
        const request = .{ .base_revision = base_revision, .source = model.bridge.source() };
        _ = model.submitJson(.preserve_pending_native_draft, .committed_input, request, 0);
    }

    fn applyEffects(model: *Model, effects: []const session_types.HostEffect, fx: anytype, autosave_message: anytype) void {
        for (effects) |effect| switch (effect.kind) {
            .schedule_autosave => if (model.autosaves.schedule(effect)) |key| {
                fx.startTimer(.{
                    .key = key,
                    .interval_ms = effect.delay_ms.?,
                    .mode = .one_shot,
                    .on_fire = autosave_message,
                });
            } else model.setStatus("Could not retain an autosave target", .{}),
            .cancel_effect => if (model.autosaves.cancel(effect.effect_id)) |key| fx.cancelTimer(key),
        };
    }

    fn setProblem(model: *Model, problem: session_types.ApplicationProblem) void {
        model.setStatus("{s}: {s}", .{ @tagName(problem.code), problem.diagnostic });
    }

    fn submitWithoutPayload(model: *Model, operation: session_mod.Operation, priority: session_mod.Priority, revision: u64) void {
        _ = model.session.submit(operation, priority, null, revision) catch {
            model.setStatus("Could not queue {s}", .{@tagName(operation)});
        };
    }

    fn submitRaw(model: *Model, operation: session_mod.Operation, payload: []const u8) void {
        _ = model.session.submit(operation, .background, payload, 0) catch {
            model.setStatus("Could not queue {s}", .{@tagName(operation)});
        };
    }

    fn nextOperationId(model: *Model, storage: []u8) ?[]const u8 {
        model.operation_serial +%= 1;
        if (model.operation_serial == 0) model.operation_serial = 1;
        return std.fmt.bufPrint(storage, "native-{d}", .{model.operation_serial}) catch {
            model.setStatus("Could not allocate an operation identifier", .{});
            return null;
        };
    }

    fn submitJson(model: *Model, operation: session_mod.Operation, priority: session_mod.Priority, value: anytype, revision: u64) ?u64 {
        const json = editor_bridge.encodeJson(std.heap.page_allocator, value) catch {
            model.setStatus("Could not encode {s}", .{@tagName(operation)});
            return null;
        };
        defer std.heap.page_allocator.free(json);
        return model.session.submit(operation, priority, json, revision) catch {
            model.setStatus("Could not queue {s}", .{@tagName(operation)});
            return null;
        };
    }
};

fn utf8PrefixLen(text: []const u8, capacity: usize) usize {
    var len = @min(text.len, capacity);
    while (len > 0 and len < text.len and text[len] & 0b1100_0000 == 0b1000_0000) len -= 1;
    return len;
}

test "only the owning edit completion may reconcile pending input" {
    var model = Model{ .session = undefined, .pending_edit_id = 22 };
    const base: session_mod.Completion = .{
        .allocator = std.testing.allocator,
        .id = 21,
        .operation = .apply_selection,
        .status = .ok,
        .response = null,
        .boundary_problem = null,
        .priority = .committed_input,
        .revision = 0,
        .payload = null,
    };
    try std.testing.expect(!model.completionOwnsPendingEdit(&base));
    var owner = base;
    owner.id = 22;
    owner.operation = .apply_text_edit;
    try std.testing.expect(model.completionOwnsPendingEdit(&owner));
}

test "deferred close completes only after replacement becomes safe" {
    var model = Model{ .session = undefined, .close_deferred = true, .replacement_safe = true };
    model.finishDeferredClose(.state, false);
    try std.testing.expect(model.close_requested);
    try std.testing.expect(!model.close_deferred);
}

test "failed deferred close save waits for an explicit retry" {
    var model = Model{
        .session = undefined,
        .close_deferred = true,
        .close_save_in_flight = true,
        .replacement_safe = false,
    };
    model.finishDeferredClose(.save, true);
    try std.testing.expect(model.close_deferred);
    try std.testing.expect(model.close_save_failed);
    try std.testing.expect(!model.close_save_in_flight);
    try std.testing.expect(!model.close_requested);
}

test "search display truncation preserves UTF-8 boundaries" {
    const title = "a" ** 255 ++ "é";
    const snippet = "b" ** 511 ++ "界";
    try std.testing.expectEqual(@as(usize, 255), utf8PrefixLen(title, 256));
    try std.testing.expectEqual(@as(usize, 511), utf8PrefixLen(snippet, 512));
    try std.testing.expect(std.unicode.utf8ValidateSlice(title[0..utf8PrefixLen(title, 256)]));
    try std.testing.expect(std.unicode.utf8ValidateSlice(snippet[0..utf8PrefixLen(snippet, 512)]));
}
