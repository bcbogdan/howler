const std = @import("std");
const session_types = @import("session_types.zig");

pub const max_autosaves: usize = 8;
const max_id_bytes: usize = 128;

pub const Entry = struct {
    used: bool = false,
    key: u64 = 0,
    effect_id_storage: [max_id_bytes]u8 = undefined,
    effect_id_len: usize = 0,
    note_id_storage: [max_id_bytes]u8 = undefined,
    note_id_len: usize = 0,
    note_kind: session_types.IdentityKind = .provisional,
    revision: u64 = 0,
    generation: u64 = 0,

    pub fn target(entry: *const Entry) session_types.SaveTarget {
        return .{
            .note_id = .{
                .kind = entry.note_kind,
                .value = entry.note_id_storage[0..entry.note_id_len],
            },
            .revision = entry.revision,
            .generation = entry.generation,
        };
    }
};

pub const Registry = struct {
    entries: [max_autosaves]Entry = @splat(.{}),

    pub fn schedule(registry: *Registry, effect: session_types.HostEffect) ?u64 {
        const target = effect.target orelse return null;
        if (effect.effect_id.len > max_id_bytes or target.note_id.value.len > max_id_bytes) return null;
        const entry = registry.findEffect(effect.effect_id) orelse registry.freeEntry() orelse return null;
        entry.* = .{ .used = true, .key = stableKey(effect.effect_id) };
        @memcpy(entry.effect_id_storage[0..effect.effect_id.len], effect.effect_id);
        entry.effect_id_len = effect.effect_id.len;
        @memcpy(entry.note_id_storage[0..target.note_id.value.len], target.note_id.value);
        entry.note_id_len = target.note_id.value.len;
        entry.note_kind = target.note_id.kind;
        entry.revision = target.revision;
        entry.generation = target.generation;
        return entry.key;
    }

    pub fn cancel(registry: *Registry, effect_id: []const u8) ?u64 {
        const entry = registry.findEffect(effect_id) orelse return null;
        const key = entry.key;
        entry.* = .{};
        return key;
    }

    pub fn takeTarget(registry: *Registry, key: u64) ?session_types.SaveTarget {
        for (&registry.entries) |*entry| {
            if (!entry.used or entry.key != key) continue;
            const target = entry.target();
            entry.used = false;
            return target;
        }
        return null;
    }

    fn findEffect(registry: *Registry, effect_id: []const u8) ?*Entry {
        for (&registry.entries) |*entry| {
            if (entry.used and std.mem.eql(u8, entry.effect_id_storage[0..entry.effect_id_len], effect_id)) return entry;
        }
        return null;
    }

    fn freeEntry(registry: *Registry) ?*Entry {
        for (&registry.entries) |*entry| if (!entry.used) return entry;
        return null;
    }
};

fn stableKey(effect_id: []const u8) u64 {
    const key = std.hash.Wyhash.hash(0x484f574c4552, effect_id);
    return if (key == 0) 1 else key;
}

test "autosave keeps the exact Rust target until its timer fires" {
    var registry = Registry{};
    const key = registry.schedule(.{
        .kind = .schedule_autosave,
        .effect_id = "autosave-1",
        .delay_ms = 750,
        .target = .{
            .note_id = .{ .kind = .provisional, .value = "note-1" },
            .revision = 4,
            .generation = 7,
        },
    }).?;
    const target = registry.takeTarget(key).?;
    try std.testing.expectEqual(@as(u64, 4), target.revision);
    try std.testing.expectEqual(@as(u64, 7), target.generation);
    try std.testing.expectEqualStrings("note-1", target.note_id.value);
    try std.testing.expect(registry.takeTarget(key) == null);
}
