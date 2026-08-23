# ADR-0003: Markdown parser and projection

Status: Accepted

Use `pulldown-cmark` with source offsets and GFM tables, strikethrough, and task lists. Parsing creates decorations and plain text only; source is never serialized from parser events. YAML front matter is inspected separately and excluded from plain-text search. Unknown and malformed syntax remains authoritative source.
