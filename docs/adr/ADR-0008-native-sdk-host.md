# ADR-0008: Native SDK host

Status: Accepted

## Decision

Replace the SwiftUI/AppKit macOS host with one Native SDK host written in Zig. Native SDK owns native rendering, text input, accessibility, windows, dialogs, timers, and platform integration. Zig translates those mechanisms into application-session ABI operations but does not reproduce application policy.

Rust remains authoritative for source, selections, revisions, undo history, persistence, recovery, conflicts, replacement safety, autosave identity, note-folder formats, and application-state schemas. The host consumes the additive application-session ABI v2 through its checked-in C header.

Howler uses a pinned Native SDK fork for controlled editor state and required macOS behavior. The dependency must name an exact commit and Zig package hash once the fork commit is remotely available; a branch reference is not acceptable.

## Compatibility

The migration preserves bundle ID `app.howler.mac`, display name `Howler`, the `Howler` Application Support directory, note-folder and recovery formats, pending-native-draft files, and Rust database schemas. Language-neutral fixtures under `apps/native/tests/fixtures` preserve reusable contracts from the deleted Swift host.

## Consequences

There is one production UI host. Git history remains the reference for the removed Swift implementation. ADR-0005 is superseded. Platform-specific behavior must enter through Native SDK rather than a second Howler-owned AppKit adapter.
