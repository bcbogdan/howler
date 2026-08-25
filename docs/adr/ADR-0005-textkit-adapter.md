# ADR-0005: TextKit adapter and macOS minimum

Status: Superseded by ADR-0008

Target macOS 13 with SwiftUI and AppKit `NSTextView`. AppKit owns input, marked text, layout, and accessibility; Rust source and revisions remain authoritative. The adapter converts `NSRange` to UTF-8 byte ranges before transactions and replaces its mirror only from accepted snapshots. TextKit 1 is selected for the initial adapter because its input and accessibility behavior is mature; TextKit 2 remains an implementation option.
