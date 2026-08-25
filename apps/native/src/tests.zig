const std = @import("std");
const native_sdk = @import("native_sdk");
const main = @import("main.zig");

const canvas = native_sdk.canvas;
const testing = std.testing;
const AppMarkup = canvas.MarkupView(main.Model, main.Msg);

test {
    _ = @import("editor_bridge.zig");
    _ = @import("effects.zig");
    _ = @import("session.zig");
    _ = @import("session_types.zig");
}

fn buildTree(arena: std.mem.Allocator, model: *const main.Model) !main.AppUi.Tree {
    var view = try AppMarkup.init(arena, main.app_markup);
    var ui = main.AppUi.init(arena);
    const node = view.build(&ui, model) catch |err| {
        if (err == error.MarkupBuild) {
            std.debug.print("app.native:{d}:{d}: {s}\n", .{ view.diagnostic.line, view.diagnostic.column, view.diagnostic.message });
        }
        return err;
    };
    return ui.finalize(node);
}

fn findText(widget: canvas.Widget, text: []const u8) bool {
    if (std.mem.eql(u8, widget.text, text)) return true;
    for (widget.children) |child| if (findText(child, text)) return true;
    return false;
}

fn findKind(widget: canvas.Widget, kind: canvas.WidgetKind) bool {
    if (widget.kind == kind) return true;
    for (widget.children) |child| if (findKind(child, kind)) return true;
    return false;
}

test "onboarding view exposes the folder action and status" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var model = main.Model{ .session = undefined };
    model.setStatus("Ready", .{});

    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(findText(tree.root, "Choose notes folder"));
    try testing.expect(findText(tree.root, "Ready"));
}

test "command mapping keeps Rust history outside the native editor" {
    try testing.expect(main.command("undo").? == .undo);
    try testing.expect(main.command("redo").? == .redo);
    try testing.expect(main.command("palette").? == .toggle_palette);
    try testing.expect(main.command("unknown") == null);
}

test "empty revision-zero active note remains editable" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var model = main.Model{ .session = undefined };
    model.capabilities_ready = true;
    model.folder_storage[0] = '/';
    model.folder_len = 1;
    model.active_editor = true;
    model.bridge.install(.{
        .revision = 0,
        .source = "",
        .selections = &.{},
        .can_undo = false,
        .can_redo = false,
    });

    const tree = try buildTree(arena_state.allocator(), &model);
    try testing.expect(model.editorEditable());
    try testing.expect(findKind(tree.root, .textarea));
    try testing.expect(!findText(tree.root, "No note is open"));
}

test "layout covers compact and default window sizes" {
    var arena_state = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena_state.deinit();
    var model = main.Model{ .session = undefined };
    const tree = try buildTree(arena_state.allocator(), &model);

    var nodes: [128]canvas.WidgetLayoutNode = undefined;
    const compact = try canvas.layoutWidgetTree(tree.root, native_sdk.geometry.RectF.init(0, 0, 640, 460), &nodes);
    try testing.expect(compact.nodes.len > 0);
    const regular = try canvas.layoutWidgetTree(tree.root, native_sdk.geometry.RectF.init(0, 0, 920, 680), &nodes);
    try testing.expect(regular.nodes.len > 0);
}
