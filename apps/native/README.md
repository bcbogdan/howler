# Howler Native Host

Howler's native-rendered Native SDK host. The view lives in `src/app.native`,
the Zig presentation layer lives in `src`, and Rust remains authoritative
through the application-session C ABI. No WebView or JavaScript runtime is
used.

Linux executable builds require GTK4 development libraries and `pkg-config`.
Headless `zig build test` does not require GTK4.

## Commands

```sh
cargo build -p howler-application-ffi # from the repository root
zig build run                         # build and launch the app
zig build test                        # run the host tests
native markup check src/app.native   # validate the markup without building
```

## Hot reload

`src/app.native` is embedded into the binary and watched during development:
edit it while the app runs and the window updates within ~2s without
losing model state. Parse failures keep the last good view.

## Native SDK pin

`build.zig.zon` points the `native_sdk` dependency at:

```text
../../../../../../tmp/opencode/howler-native-sdk
```

The local path is temporary. It contains controlled-editor commit `e6a9b595`
and macOS window-behavior commit `8a5d711e`. Push the fork, then replace this
path with an exact remote URL and package hash; do not point Howler at a branch.
