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

`build.zig.zon` pins the `native_sdk` dependency to:

```text
https://github.com/bcbogdan/native/commit/fe1c27994c2948fc0b9faf84c2222f7d5981621b
```

The Zig package hash is
`native_sdk-0.1.0-hzDzQu5IsQLrHwJDFjYxbPaqJiCe4Ai4DwHbFL-NAFUp`. Keep both
the commit and package hash exact; do not point Howler at a branch.
