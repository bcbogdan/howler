const std = @import("std");

const c = @cImport({
    @cInclude("howler_application.h");
});

pub const Operation = enum {
    state,
    capabilities,
    connect,
    adopt_folder,
    create_note,
    open_note,
    close_note,
    apply_text_edit,
    apply_selection,
    preserve_pending_native_draft,
    resolve_pending_native_draft,
    execute_command,
    undo,
    redo,
    save,
    resolve_conflict,
    restore_recovery,
    discard_recovery,
    reconcile_active,
    search,
    rename_note,
    move_note,
    trash_note,
    restore_note,
    diagnostics,
    diagnostic_bundle,
};

pub const Priority = enum { committed_input, background };

pub const TransportStatus = enum {
    ok,
    invalid_argument,
    busy,
    internal,
    unexpected,

    fn fromCode(code: c_int) TransportStatus {
        return switch (code) {
            c.HOWLER_APPLICATION_OK => .ok,
            c.HOWLER_APPLICATION_INVALID_ARGUMENT => .invalid_argument,
            c.HOWLER_APPLICATION_BUSY => .busy,
            c.HOWLER_APPLICATION_INTERNAL => .internal,
            else => .unexpected,
        };
    }
};

pub const Completion = struct {
    allocator: std.mem.Allocator,
    id: u64,
    operation: Operation,
    status: TransportStatus,
    response: ?[]u8,
    boundary_problem: ?[]u8,
    priority: Priority,
    revision: u64,
    payload: ?[:0]u8,

    pub fn deinit(completion: *Completion) void {
        if (completion.response) |response| completion.allocator.free(response);
        if (completion.boundary_problem) |problem| completion.allocator.free(problem);
        if (completion.payload) |payload| completion.allocator.free(payload);
        completion.* = undefined;
    }
};

const Request = struct {
    allocator: std.mem.Allocator,
    id: u64,
    operation: Operation,
    payload: ?[:0]u8,
    revision: u64,
    priority: Priority,

    fn init(allocator: std.mem.Allocator, id: u64, operation: Operation, priority: Priority, payload: ?[]const u8, revision: u64) !Request {
        return .{
            .allocator = allocator,
            .id = id,
            .operation = operation,
            .payload = if (payload) |bytes| try allocator.dupeZ(u8, bytes) else null,
            .revision = revision,
            .priority = priority,
        };
    }

    fn deinit(request: *Request) void {
        if (request.payload) |payload| request.allocator.free(payload);
        request.* = undefined;
    }
};

fn Queue(comptime T: type, comptime capacity: usize) type {
    return struct {
        items: [capacity]T = undefined,
        count: usize = 0,

        fn push(queue: *@This(), item: T) error{QueueFull}!void {
            if (queue.count == capacity) return error.QueueFull;
            queue.items[queue.count] = item;
            queue.count += 1;
        }

        fn pop(queue: *@This()) ?T {
            if (queue.count == 0) return null;
            const item = queue.items[0];
            if (queue.count > 1) {
                std.mem.copyForwards(T, queue.items[0 .. queue.count - 1], queue.items[1..queue.count]);
            }
            queue.count -= 1;
            return item;
        }
    };
}

const RequestQueue = Queue(Request, 32);
const CompletionQueue = Queue(Completion, 32);

pub const Session = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    handle: *c.HowlerApplicationSession,
    worker: std.Thread,
    mutex: std.Io.Mutex = .init,
    request_ready: std.Io.Condition = .init,
    completion_space: std.Io.Condition = .init,
    committed_input: RequestQueue = .{},
    background: RequestQueue = .{},
    completions: CompletionQueue = .{},
    next_id: u64 = 1,
    stopping: bool = false,
    committed_streak: usize = 0,

    pub fn create(allocator: std.mem.Allocator, io: std.Io) !*Session {
        var handle: ?*c.HowlerApplicationSession = null;
        if (c.howler_session_create(&handle) != c.HOWLER_APPLICATION_OK or handle == null) {
            return error.SessionCreateFailed;
        }
        errdefer c.howler_session_destroy(handle);

        const session = try allocator.create(Session);
        errdefer allocator.destroy(session);
        session.* = .{
            .allocator = allocator,
            .io = io,
            .handle = handle.?,
            .worker = undefined,
        };
        session.worker = try std.Thread.spawn(.{}, workerMain, .{session});
        return session;
    }

    pub fn destroy(session: *Session) void {
        session.mutex.lockUncancelable(session.io);
        session.stopping = true;
        session.request_ready.broadcast(session.io);
        session.completion_space.broadcast(session.io);
        session.mutex.unlock(session.io);
        session.worker.join();

        while (session.committed_input.pop()) |request_value| {
            var request = request_value;
            request.deinit();
        }
        while (session.background.pop()) |request_value| {
            var request = request_value;
            request.deinit();
        }
        while (session.completions.pop()) |completion_value| {
            var completion = completion_value;
            completion.deinit();
        }
        c.howler_session_destroy(session.handle);
        const allocator = session.allocator;
        allocator.destroy(session);
    }

    pub fn submit(
        session: *Session,
        operation: Operation,
        priority: Priority,
        payload: ?[]const u8,
        revision: u64,
    ) !u64 {
        session.mutex.lockUncancelable(session.io);
        defer session.mutex.unlock(session.io);
        if (session.stopping) return error.SessionStopping;

        const id = session.next_id;
        session.next_id +%= 1;
        if (session.next_id == 0) session.next_id = 1;
        var request = try Request.init(session.allocator, id, operation, priority, payload, revision);
        errdefer request.deinit();
        switch (priority) {
            .committed_input => try session.committed_input.push(request),
            .background => try session.background.push(request),
        }
        session.request_ready.signal(session.io);
        return id;
    }

    pub fn poll(session: *Session) ?Completion {
        session.mutex.lockUncancelable(session.io);
        defer session.mutex.unlock(session.io);
        const completion = session.completions.pop() orelse return null;
        session.completion_space.signal(session.io);
        return completion;
    }

    fn workerMain(session: *Session) void {
        while (true) {
            session.mutex.lockUncancelable(session.io);
            while (!session.stopping and session.committed_input.count == 0 and session.background.count == 0) {
                session.request_ready.waitUncancelable(session.io, &session.mutex);
            }
            if (session.stopping) {
                session.mutex.unlock(session.io);
                return;
            }
            var request = if (session.committed_input.count > 0 and
                (session.background.count == 0 or session.committed_streak < 8))
            request: {
                session.committed_streak += 1;
                break :request session.committed_input.pop().?;
            } else request: {
                session.committed_streak = 0;
                break :request session.background.pop().?;
            };
            session.mutex.unlock(session.io);

            var completion = session.execute(&request);
            request.deinit();

            session.mutex.lockUncancelable(session.io);
            while (!session.stopping and session.completions.count == 32) {
                session.completion_space.waitUncancelable(session.io, &session.mutex);
            }
            if (session.stopping) {
                session.mutex.unlock(session.io);
                completion.deinit();
                return;
            }
            session.completions.push(completion) catch unreachable;
            session.mutex.unlock(session.io);
        }
    }

    fn execute(session: *Session, request: *Request) Completion {
        var response: [*c]u8 = null;
        var boundary: [*c]u8 = null;
        const payload: [*c]const u8 = if (request.payload) |value| value.ptr else null;
        const status = switch (request.operation) {
            .state => c.howler_session_state_json(session.handle, &response, &boundary),
            .capabilities => c.howler_session_capabilities_json(session.handle, &response, &boundary),
            .connect => c.howler_session_connect_json(session.handle, payload, &response, &boundary),
            .adopt_folder => c.howler_session_adopt_folder_json(session.handle, &response, &boundary),
            .create_note => c.howler_session_create_note_json(session.handle, payload, &response, &boundary),
            .open_note => c.howler_session_open_note_json(session.handle, payload, &response, &boundary),
            .close_note => c.howler_session_close_note_json(session.handle, &response, &boundary),
            .apply_text_edit => c.howler_session_apply_text_edit_json(session.handle, payload, &response, &boundary),
            .apply_selection => c.howler_session_apply_selection_json(session.handle, payload, &response, &boundary),
            .preserve_pending_native_draft => c.howler_session_preserve_pending_native_draft_json(session.handle, payload, &response, &boundary),
            .resolve_pending_native_draft => c.howler_session_resolve_pending_native_draft_json(session.handle, payload, &response, &boundary),
            .execute_command => c.howler_session_execute_command_json(session.handle, request.revision, payload, &response, &boundary),
            .undo => c.howler_session_undo_json(session.handle, request.revision, &response, &boundary),
            .redo => c.howler_session_redo_json(session.handle, request.revision, &response, &boundary),
            .save => c.howler_session_save_json(session.handle, payload, &response, &boundary),
            .resolve_conflict => c.howler_session_resolve_conflict_json(session.handle, payload, &response, &boundary),
            .restore_recovery => c.howler_session_restore_recovery_json(session.handle, payload, &response, &boundary),
            .discard_recovery => c.howler_session_discard_recovery_json(session.handle, payload, &response, &boundary),
            .reconcile_active => c.howler_session_reconcile_active_json(session.handle, &response, &boundary),
            .search => c.howler_session_search_json(session.handle, payload, &response, &boundary),
            .rename_note => c.howler_session_rename_note_json(session.handle, payload, &response, &boundary),
            .move_note => c.howler_session_move_note_json(session.handle, payload, &response, &boundary),
            .trash_note => c.howler_session_trash_note_json(session.handle, payload, &response, &boundary),
            .restore_note => c.howler_session_restore_note_json(session.handle, payload, &response, &boundary),
            .diagnostics => c.howler_session_diagnostics_json(session.handle, &response, &boundary),
            .diagnostic_bundle => c.howler_session_diagnostic_bundle_json(session.handle, &response, &boundary),
        };
        const retry_payload = request.payload;
        request.payload = null;
        return .{
            .allocator = session.allocator,
            .id = request.id,
            .operation = request.operation,
            .status = TransportStatus.fromCode(status),
            .response = copyAndFree(session.allocator, response),
            .boundary_problem = copyAndFree(session.allocator, boundary),
            .priority = request.priority,
            .revision = request.revision,
            .payload = retry_payload,
        };
    }
};

fn copyAndFree(allocator: std.mem.Allocator, value: [*c]u8) ?[]u8 {
    if (value == null) return null;
    defer c.howler_session_string_free(value);
    const source = std.mem.span(@as([*:0]const u8, @ptrCast(value)));
    return allocator.dupe(u8, source) catch null;
}

test "worker owns one session and returns copied responses" {
    const io = std.testing.io;
    const session = try Session.create(std.testing.allocator, io);
    defer session.destroy();

    const id = try session.submit(.capabilities, .background, null, 0);
    while (true) {
        if (session.poll()) |completion_value| {
            var completion = completion_value;
            defer completion.deinit();
            try std.testing.expectEqual(id, completion.id);
            try std.testing.expectEqual(TransportStatus.ok, completion.status);
            try std.testing.expect(completion.response != null);
            break;
        }
        std.Thread.yield() catch {};
    }
}

fn waitForTestCompletion(session: *Session, id: u64) !Completion {
    while (true) {
        if (session.poll()) |completion| {
            if (completion.id == id) return completion;
            var unexpected = completion;
            unexpected.deinit();
            return error.UnexpectedCompletion;
        }
        std.Thread.yield() catch {};
    }
}

fn encodeTestJson(allocator: std.mem.Allocator, value: anytype) ![]u8 {
    var output: std.Io.Writer.Allocating = .init(allocator);
    errdefer output.deinit();
    var stringify: std.json.Stringify = .{ .writer = &output.writer };
    try stringify.write(value);
    return output.toOwnedSlice();
}

test "real ABI connects edits selects saves searches and replays history" {
    const types = @import("session_types.zig");
    const io = std.testing.io;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.createDirPath(io, "notes");
    try tmp.dir.createDirPath(io, "state");

    var notes_storage: [256]u8 = undefined;
    var state_storage: [256]u8 = undefined;
    const notes_path = try std.fmt.bufPrint(&notes_storage, ".zig-cache/tmp/{s}/notes", .{tmp.sub_path[0..]});
    const state_path = try std.fmt.bufPrint(&state_storage, ".zig-cache/tmp/{s}/state", .{tmp.sub_path[0..]});

    const session = try Session.create(std.testing.allocator, io);
    defer session.destroy();

    const request = try encodeTestJson(std.testing.allocator, .{
        .folder_path = notes_path,
        .application_state_path = state_path,
        .adopt = false,
        .create_missing = false,
    });
    defer std.testing.allocator.free(request);
    var completion = try waitForTestCompletion(session, try session.submit(.connect, .background, request, 0));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();

    completion = try waitForTestCompletion(session, try session.submit(.create_note, .background, "{\"source\":null}", 0));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();

    const edit_json =
        \\{"expected_revision":0,"replacements":[{"range":{"start":0,"end":0},"text":"hello"}],"selections":[{"anchor":5,"head":5,"affinity":"Downstream","revision":1}],"history":"Typing","composition":null,"input_origin":"typing"}
    ;
    completion = try waitForTestCompletion(session, try session.submit(.apply_text_edit, .committed_input, edit_json, 0));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();

    const selection_json =
        \\{"expected_revision":1,"selections":[{"anchor":5,"head":0,"affinity":"Upstream","revision":1}]}
    ;
    completion = try waitForTestCompletion(session, try session.submit(.apply_selection, .committed_input, selection_json, 1));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();

    completion = try waitForTestCompletion(session, try session.submit(.undo, .committed_input, null, 1));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();
    completion = try waitForTestCompletion(session, try session.submit(.redo, .committed_input, null, 2));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();

    completion = try waitForTestCompletion(session, try session.submit(.state, .background, null, 0));
    var arena_state = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena_state.deinit();
    const state_response = try types.decodeResponse(types.EmptyResult, arena_state.allocator(), completion.response.?);
    const active = state_response.state.active.?;
    try std.testing.expectEqualStrings("hello", active.editor.snapshot.source);
    const save_json = try encodeTestJson(std.testing.allocator, .{
        .note_id = active.note_id,
        .revision = active.editor.snapshot.revision,
        .generation = active.generation,
    });
    completion.deinit();
    defer std.testing.allocator.free(save_json);

    completion = try waitForTestCompletion(session, try session.submit(.save, .background, save_json, 0));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    completion.deinit();
    completion = try waitForTestCompletion(session, try session.submit(.search, .background, "{\"query\":\"hello\",\"limit\":10}", 0));
    try std.testing.expectEqual(TransportStatus.ok, completion.status);
    try std.testing.expect(std.mem.indexOf(u8, completion.response.?, "hello") != null);
    completion.deinit();
}
