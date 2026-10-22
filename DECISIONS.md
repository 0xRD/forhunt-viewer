# Decisions

Every ambiguous point resolved during the build: what, why, and the measured
evidence where there is some. Newest last.

| # | Decision | Reason / evidence |
|---|----------|-------------------|
| D-001 | No Apple Developer ID, no Windows code-signing certificate: macOS .dmg and Windows .msi/.zip ship unsigned. README documents right-click > Open (macOS) and the SmartScreen "More info > Run anyway" (Windows). | User decision (2026-10-22). Recorded as B-001/B-002 so signing can be added when credentials exist. |
| D-002 | Homebrew cask: template written now, published after v0.1.0. | User decision (2026-10-22); needs a tap token (B-003). |
| D-003 | Crates are `fhv-engine` (lib), `fhv-app` (bin `forhunt-viewer`), `csv-gen` (tool), edition 2024, licence `MIT OR Apache-2.0`. | Brief. Prefixed names avoid clashes on crates.io and with std crate names. |
| D-004 | Subagents: research/review on Sonnet, mechanical tasks on Haiku; the engine core stays on the main agent; no Opus subagents. | User instruction (cost). |
