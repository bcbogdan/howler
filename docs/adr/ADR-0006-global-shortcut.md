# ADR-0006: Global shortcut

Status: Accepted

Register Command-Option-H through Carbon `RegisterEventHotKey`, which does not require accessibility permission. Registration failure leaves menu activation available and must be visible in Settings. Store future customization in host preferences. Carbon is isolated behind `GlobalShortcut` so a newer supported API can replace it without changing application services.
