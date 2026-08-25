const std = @import("std");
const native_sdk = @import("native_sdk");

pub fn build(b: *std.Build) void {
    const artifacts = native_sdk.addAppArtifacts(b, b.dependency("native_sdk", .{}), .{
        .name = "howler",
        .manifest = "app.json",
    });
    const cargo = b.addSystemCommand(&.{ "cargo", "build", "-p", "howler-application-ffi" });
    cargo.setCwd(b.path("../.."));
    const rust_archive = b.path("../../target/debug/libhowler_application_ffi.a");
    for ([_]*std.Build.Step.Compile{ artifacts.exe, artifacts.tests }) |artifact| {
        artifact.step.dependOn(&cargo.step);
        artifact.root_module.addIncludePath(b.path("../../ffi/application/include"));
        artifact.root_module.addObjectFile(rust_archive);
        artifact.root_module.link_libc = true;
        if (artifact.root_module.resolved_target.?.result.os.tag == .linux) {
            artifact.root_module.linkSystemLibrary("gcc_s", .{});
            artifact.root_module.linkSystemLibrary("util", .{});
            artifact.root_module.linkSystemLibrary("rt", .{});
            artifact.root_module.linkSystemLibrary("pthread", .{});
            artifact.root_module.linkSystemLibrary("m", .{});
            artifact.root_module.linkSystemLibrary("dl", .{});
        }
    }
    artifacts.tests.root_module.addAnonymousImport("application-response.json", .{
        .root_source_file = b.path("tests/fixtures/application-response.json"),
    });
    artifacts.tests.root_module.addAnonymousImport("unknown-required-enum.json", .{
        .root_source_file = b.path("tests/fixtures/unknown-required-enum.json"),
    });
}
