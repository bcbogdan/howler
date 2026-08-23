# ADR-0002: Buffer and ranges

Status: Accepted

Use `ropey::Rope` as authoritative storage and UTF-8 byte offsets at code-point boundaries as public coordinates. Apply non-overlapping replacements from highest offset to lowest. Store inverse replacement text, not complete snapshots, in deterministic history groups. Hosts explicitly convert native UTF-16 ranges.
