# ADR-0001: Workspace and boundaries

Status: Accepted

Use one dependency-pure `howler-editor` crate and one `howler-app` crate for Milestone 1. Keep the standalone editor free of filesystem, SQLite, platform, and Howler identity types. Separate editor and application ABI crates depend on only their respective layers. Further splits are deferred until dependency boundaries justify them.
