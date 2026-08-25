const std = @import("std");
const builtin = @import("builtin");
const runner = @import("runner");
const native_sdk = @import("native_sdk");
const model_mod = @import("model.zig");
const platform = @import("platform.zig");
const session_mod = @import("session.zig");

pub const panic = std.debug.FullPanic(native_sdk.debug.capturePanic);

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const app_dirs = native_sdk.app_dirs;
const canvas_label = "main-canvas";
const window_width: f32 = 920;
const window_height: f32 = 680;
const poll_timer_key: u64 = 1;

const app_permissions = [_][]const u8{
    native_sdk.security.permission_command,
    native_sdk.security.permission_dialog,
    native_sdk.security.permission_view,
};
const shell_views = [_]native_sdk.ShellView{.{
    .label = canvas_label,
    .kind = .gpu_surface,
    .fill = true,
    .role = "Howler editor",
    .accessibility_label = "Howler",
    .gpu_backend = .metal,
    .gpu_pixel_format = .bgra8_unorm,
    .gpu_present_mode = .timer,
    .gpu_alpha_mode = .@"opaque",
    .gpu_color_space = .srgb,
    .gpu_vsync = true,
}};
const shell_windows = [_]native_sdk.ShellWindow{.{
    .label = "main",
    .title = platform.display_name,
    .width = window_width,
    .height = window_height,
    .min_width = 640,
    .min_height = 460,
    .titlebar = .hidden_inset_tall,
    .always_on_top = true,
    .can_join_all_spaces = true,
    .full_screen_auxiliary = true,
    .close_policy = .event,
    .views = &shell_views,
}};
const shell_scene: native_sdk.ShellConfig = .{ .windows = &shell_windows };

const shortcuts = [_]native_sdk.Shortcut{
    .{ .id = "new-note", .key = "n", .modifiers = .{ .primary = true } },
    .{ .id = "palette", .key = "p", .modifiers = .{ .primary = true } },
    .{ .id = "save", .key = "s", .modifiers = .{ .primary = true } },
    .{ .id = "undo", .key = "z", .modifiers = .{ .primary = true } },
    .{ .id = "redo", .key = "z", .modifiers = .{ .primary = true, .shift = true } },
};

pub const Msg = union(enum) {
    choose_folder,
    create_note,
    toggle_palette,
    dismiss,
    edit_query: canvas.TextInputEvent,
    open_search_result: usize,
    editor_event: canvas.EditorEvent,
    save,
    undo,
    redo,
    request_close,
    discard_pending_draft,
    save_pending_draft_as_new,
    use_external_conflict,
    keep_conflict_as_new,
    restore_recovery,
    discard_recovery,
    session_tick: native_sdk.EffectTimer,
    autosave_tick: native_sdk.EffectTimer,

    pub const view_unbound = .{ "session_tick", "autosave_tick", "editor_event", "undo", "redo", "request_close" };
};

pub const Model = model_mod.Model;
const HowlerApp = native_sdk.UiAppWithFeatures(Model, Msg, .{ .runtime_markup = builtin.mode == .Debug });
pub const Effects = HowlerApp.Effects;
pub const AppUi = canvas.Ui(Msg);
pub const app_markup = @embedFile("app.native");

pub fn boot(model: *Model, fx: *Effects) void {
    model.requestCapabilities();
    fx.startTimer(.{
        .key = poll_timer_key,
        .interval_ms = 16,
        .mode = .repeating,
        .on_fire = Effects.timerMsg(.session_tick),
    });
}

pub fn update(model: *Model, msg: Msg, fx: *Effects) void {
    switch (msg) {
        .choose_folder => model.chooseFolder(),
        .create_note => model.createNote(),
        .toggle_palette => model.togglePalette(),
        .dismiss => model.dismissPalette(),
        .edit_query => |event| model.editQuery(event),
        .open_search_result => |index| model.openSearchResult(index),
        .editor_event => |event| model.handleEditorEvent(event),
        .save => model.save(),
        .undo => model.undo(),
        .redo => model.redo(),
        .request_close => model.requestClose(),
        .discard_pending_draft => model.discardPendingDraft(),
        .save_pending_draft_as_new => model.savePendingDraftAsNew(),
        .use_external_conflict => model.useExternalConflict(),
        .keep_conflict_as_new => model.keepConflictAsNew(),
        .restore_recovery => model.restoreRecovery(),
        .discard_recovery => model.discardRecovery(),
        .session_tick => |timer| if (timer.outcome == .fired) {
            model.processCompletions(fx, Effects.timerMsg(.autosave_tick));
        },
        .autosave_tick => |timer| if (timer.outcome == .fired) model.autosaveFired(timer.key),
    }
}

pub fn command(name: []const u8) ?Msg {
    if (std.mem.eql(u8, name, "new-note")) return .create_note;
    if (std.mem.eql(u8, name, "palette")) return .toggle_palette;
    if (std.mem.eql(u8, name, "save")) return .save;
    if (std.mem.eql(u8, name, "undo")) return .undo;
    if (std.mem.eql(u8, name, "redo")) return .redo;
    if (std.mem.eql(u8, name, "window.close_requested")) return .request_close;
    return null;
}

fn appOptions(io: std.Io) HowlerApp.Options {
    return .{
        .name = "howler",
        .scene = shell_scene,
        .canvas_label = canvas_label,
        .update_fx = update,
        .init_fx = boot,
        .on_command = command,
        .markup = if (builtin.mode == .Debug)
            .{ .source = app_markup, .watch_path = "src/app.native", .io = io }
        else
            null,
    };
}

const HowlerHost = struct {
    ui_app: *HowlerApp,
    allocator: std.mem.Allocator,
    io: std.Io,
    env: app_dirs.Env,
    handled_folder_picker_serial: u64 = 0,

    fn init(allocator: std.mem.Allocator, io: std.Io, env: app_dirs.Env, session: *session_mod.Session) !HowlerHost {
        const ui_app = try allocator.create(HowlerApp);
        errdefer allocator.destroy(ui_app);
        ui_app.* = HowlerApp.init(allocator, Model.init(session), appOptions(io));
        return .{ .ui_app = ui_app, .allocator = allocator, .io = io, .env = env };
    }

    fn deinit(host: *HowlerHost) void {
        host.ui_app.deinit();
        host.allocator.destroy(host.ui_app);
    }

    fn app(host: *HowlerHost) native_sdk.App {
        return .{
            .context = host,
            .name = "howler",
            .scene_fn = scene,
            .event_fn = event,
            .stop_fn = stop,
        };
    }

    fn scene(_: *anyopaque) anyerror!native_sdk.ShellConfig {
        return shell_scene;
    }

    fn event(context: *anyopaque, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) anyerror!void {
        const host: *HowlerHost = @ptrCast(@alignCast(context));
        try host.ui_app.app().event(runtime, event_value);
        try host.presentFolderDialog(runtime);
        host.performClose(runtime);
    }

    fn stop(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const host: *HowlerHost = @ptrCast(@alignCast(context));
        try host.ui_app.app().stop(runtime);
    }

    fn presentFolderDialog(host: *HowlerHost, runtime: *native_sdk.Runtime) !void {
        const serial = host.ui_app.model.folder_picker_serial;
        if (serial == host.handled_folder_picker_serial) return;
        host.handled_folder_picker_serial = serial;

        var path_buffer: [native_sdk.platform.max_dialog_paths_bytes]u8 = undefined;
        const result = runtime.showOpenDialog(.{
            .title = "Choose notes folder",
            .default_path = host.ui_app.model.folderPath(),
            .allow_directories = true,
            .allow_multiple = false,
        }, &path_buffer) catch {
            host.ui_app.model.setStatus("The folder dialog could not be opened", .{});
            return;
        };
        if (result.count == 0) return;

        var state_path_buffer: [1024]u8 = undefined;
        const state_path = app_dirs.resolveOne(
            .{ .name = platform.application_support_directory },
            app_dirs.currentPlatform(),
            host.env,
            .data,
            &state_path_buffer,
        ) catch {
            host.ui_app.model.setStatus("The Howler application-state path is unavailable", .{});
            return;
        };
        host.ui_app.model.connect(result.paths, state_path);
    }

    fn performClose(host: *HowlerHost, runtime: *native_sdk.Runtime) void {
        if (!host.ui_app.model.close_requested) return;
        runtime.hideWindow(1) catch return;
        host.ui_app.model.close_requested = false;
    }
};

pub fn main(init: std.process.Init) !void {
    const session = try session_mod.Session.create(std.heap.page_allocator, init.io);
    defer session.destroy();

    var host = try HowlerHost.init(
        std.heap.page_allocator,
        init.io,
        native_sdk.debug.envFromMap(init.environ_map),
        session,
    );
    defer host.deinit();

    try runner.runWithOptions(host.app(), .{
        .app_name = "howler",
        .window_title = platform.display_name,
        .bundle_id = platform.bundle_id,
        .icon_path = "assets/icon.png",
        .default_frame = geometry.RectF.init(0, 0, window_width, window_height),
        .shortcuts = &shortcuts,
        .js_window_api = false,
        .security = .{
            .permissions = &app_permissions,
            .navigation = .{ .allowed_origins = &.{ "zero://inline", "zero://app" } },
        },
    }, init);
}

test {
    _ = @import("tests.zig");
}
