const std = @import("std");

pub const Affinity = enum { Upstream, Downstream };
pub const OutcomeStatus = enum { applied, rejected };
pub const ProblemCode = enum {
    not_connected,
    note_not_found,
    recovery_not_found,
    recovery_pending,
    stale_revision,
    external_conflict,
    identity_changed,
    stale_editor,
    wrong_owner,
    destination_exists,
    duplicate_identity,
    invalid_operation,
    persistence_failure,
    task_not_found,
    content_hash_mismatch,
    adoption_required,
    database_failure,
};
pub const ReplacementSafety = enum { safe, must_retain_editor };
pub const Durability = enum { accepted, recovery_durable, file_saved };
pub const IdentityKind = enum { provisional, adopted };
pub const EffectKind = enum { schedule_autosave, cancel_effect };

pub const Identity = struct {
    kind: IdentityKind,
    value: []const u8,
};

pub const Selection = struct {
    anchor: usize,
    head: usize,
    affinity: Affinity,
    revision: u64,
};

pub const Snapshot = struct {
    revision: u64,
    source: []const u8,
    selections: []const Selection,
    can_undo: bool,
    can_redo: bool,
};

pub const FolderState = struct {
    path: []const u8,
    adopted: bool,
    generation: u64,
};

pub const PersistenceState = struct {
    durability: Durability,
    replacement_safety: ReplacementSafety,
    issues: []const std.json.Value,
};

pub const PendingNativeDraftState = struct {
    base_revision: u64,
    source: []const u8,
    durable: bool,
};

pub const ConflictState = struct {
    external_source: []const u8,
    external_hash: []const u8,
};

pub const DecorationSet = struct {
    revision: u64,
    items: []const std.json.Value,
};

pub const EditorPresentationState = struct {
    snapshot: Snapshot,
    decorations: DecorationSet,
};

pub const ActiveEditorState = struct {
    note_id: Identity,
    editor: EditorPresentationState,
    persistence: PersistenceState,
    conflict: ?ConflictState,
    pending_native_draft: ?PendingNativeDraftState,
    generation: u64,
};

pub const ApplicationState = struct {
    folder: ?FolderState,
    active: ?ActiveEditorState,
    recoveries: []const RecoveryDraft,
    background_tasks: []const std.json.Value,
};

pub const RecoveryDraft = struct {
    note_id: []const u8,
    relative_path: []const u8,
    revision: u64,
    base_hash: []const u8,
    source: []const u8,
};

pub const MatchReason = enum { exact_title, prefix_title, fuzzy_title, body, recent };

pub const NoteSummary = struct {
    id: Identity,
    relative_path: []const u8,
    title: []const u8,
    content_hash: []const u8,
};

pub const SearchResult = struct {
    note: NoteSummary,
    snippet: []const u8,
    reason: MatchReason,
};

pub const SaveTarget = struct {
    note_id: Identity,
    revision: u64,
    generation: u64,
};

pub const HostEffect = struct {
    kind: EffectKind,
    effect_id: []const u8,
    delay_ms: ?u64 = null,
    target: ?SaveTarget = null,
};

pub const ApplicationProblem = struct {
    code: ProblemCode,
    diagnostic: []const u8,
    details: ?std.json.Value,
};

pub const SessionCapabilities = struct {
    application_session_abi: u32,
    selection_updates: bool,
    input_origin_metadata: bool,
    rust_owned_history: bool,
    pending_native_drafts: bool,
};

pub const ConnectResult = struct {
    opened_note: ?std.json.Value,
};

pub const EmptyResult = ?u8;

pub fn OperationOutcome(comptime T: type) type {
    return union(enum) {
        applied: T,
        rejected: ApplicationProblem,
    };
}

pub fn ApplicationResponse(comptime T: type) type {
    return struct {
        state: ApplicationState,
        effects: []const HostEffect,
        outcome: OperationOutcome(T),
    };
}

const RawOutcome = struct {
    status: OutcomeStatus,
    value: std.json.Value,
};

const RawResponse = struct {
    state: ApplicationState,
    effects: []const HostEffect,
    outcome: RawOutcome,
};

pub fn decodeResponse(comptime T: type, arena: std.mem.Allocator, source: []const u8) !ApplicationResponse(T) {
    const options: std.json.ParseOptions = .{ .ignore_unknown_fields = true, .allocate = .alloc_always };
    const raw = try std.json.parseFromSliceLeaky(RawResponse, arena, source, options);
    for (raw.effects) |effect| switch (effect.kind) {
        .schedule_autosave => if (effect.delay_ms == null or effect.target == null) return error.MissingEffectField,
        .cancel_effect => {},
    };
    return .{
        .state = raw.state,
        .effects = raw.effects,
        .outcome = switch (raw.outcome.status) {
            .applied => .{ .applied = try std.json.parseFromValueLeaky(T, arena, raw.outcome.value, options) },
            .rejected => .{ .rejected = try std.json.parseFromValueLeaky(ApplicationProblem, arena, raw.outcome.value, options) },
        },
    };
}

test "decodes authoritative state and identified effect" {
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();

    const response = try decodeResponse(ConnectResult, arena_state.allocator(), @embedFile("application-response.json"));
    try std.testing.expectEqual(@as(u64, 3), response.state.folder.?.generation);
    try std.testing.expectEqualStrings("autosave-1", response.effects[0].effect_id);
    try std.testing.expectEqual(@as(u64, 750), response.effects[0].delay_ms.?);
    try std.testing.expect(response.outcome == .applied);
}

test "rejects an unknown required problem code" {
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();

    try std.testing.expectError(
        error.InvalidEnumTag,
        decodeResponse(ConnectResult, arena_state.allocator(), @embedFile("unknown-required-enum.json")),
    );
}

test "tolerates unknown optional object fields" {
    const source =
        \\{"state":{"folder":null,"active":null,"recoveries":[],"background_tasks":[],"future":true},"effects":[],"outcome":{"status":"applied","value":null},"future":{}}
    ;
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();

    const response = try decodeResponse(EmptyResult, arena_state.allocator(), source);
    try std.testing.expect(response.outcome == .applied);
}

test "rejects an incomplete autosave effect" {
    const source =
        \\{"state":{"folder":null,"active":null,"recoveries":[],"background_tasks":[]},"effects":[{"kind":"schedule_autosave","effect_id":"missing-target","delay_ms":10}],"outcome":{"status":"applied","value":null}}
    ;
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();
    try std.testing.expectError(error.MissingEffectField, decodeResponse(EmptyResult, arena_state.allocator(), source));
}
