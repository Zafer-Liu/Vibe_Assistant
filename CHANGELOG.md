# Changelog

All notable changes to this project are documented in this file. Each version
lists the changes from the interval between its release tag and the previous
one. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [v1.3.1] - 2026-11-18

### Added

- Baidu Netdisk as a cloud vault target: local OpenList deployment, authorization, mounting, and vault synchronization.

### Changed

- Improved browser URL activation and remote sync handling; documentation updates.

## [v1.3.0] - 2026-11-17

### Added

- Reviewable memory refinement: label-scoped refinement, generated conflict proposals, refinement history, and undo. L1 replacement identities, label redirects, and cloud deletion state are preserved.
- Retrieval tooling: shared retrieval, source-evidence access, tracing, and synthetic evaluation.

### Changed

- Bounded semantic work; optimized paged reads, virtual lists, and page loading.
- Hardened transcript extraction and MCP JSONL encoding and flushing.
- Documented workflows; added reusable synthetic UI verification fixtures.

## [v1.2.4] - 2026-11-07

### Added

- Validation of cloud usage ledger corrections: signed Codex counter corrections are preserved, session-level token totals validated, and field-specific errors reported without exposing record IDs. Anonymous session identities are stable across exports and imports.

### Changed

- Telemetry analytics: calendar-specific usage queries with shared cached snapshots, telemetry work moved off the UI thread, and coalesced frontend requests.
- Transcript recovery: improved recovery of missing native transcripts while preserving read errors and safety limits.
- MCP observation and startup flows: stable client bindings separated from probe generations, safe probe publication and recovery, improved startup readiness and memory loading, and reconfiguration guidance for incompatible MCP arguments. Unified command resolution, runtime publication, telemetry tracking, and database migration behavior.

## [v1.2.3] - 2026-11-06

### Added

- Precise cloud usage ledger sync: detailed token usage records synced across devices, with legacy daily aggregates preserved for older clients. Analytics distinguish daily-only totals and prevent duplicate counting.

### Changed

- Improved remote request timeouts and error reporting; refresh events emitted after sync.
- Hardened the release workflow: tag and commit validation, draft releases with scoped permissions, installer uploads, and artifact preservation on upload failures.
- CI and release installs skip unnecessary ONNX Runtime downloads (WebView inference uses onnxruntime-web).

## [v1.2.2] - 2026-11-05

### Added

- Optional model configuration sync over WebDAV/S3: model connections, encrypted API keys, decision models, and semantic embedding settings, with cross-device merge, deletion tombstones, and local key resealing.
- Language selection as the first onboarding step on first launch; the interface switches immediately and the step is skipped when a language is already chosen.
- Confirmation guard before destructive environment variable changes.

### Changed

- UI modernization: reusable modal and confirmation components replace native dialogs; improved responsive layouts, theme handling, accessibility, typography, and localized strings.
- Windows installer branding: 16–256px icon ladder, NSIS installer and uninstaller icons plus header and sidebar bitmaps; same-version reinstall and upgrade now overwrite in place instead of asking to uninstall first.

### Fixed

- Cloud classification replay ordering: memory objects are applied before tag overrides, completed checkpoints and manual tags are preserved, and affected sync metadata is replayed once during database initialization.

## [v1.2.1] - 2026-11-05

### Added

- Full extracted memory aggregates in per-device cloud mirrors: session-scoped extracted memories with durability labels; fixed size limits for memory and usage aggregates removed.
- Expanded cloud memory sync coverage: memory domains, provenance, timestamps, user state, and receiver retagging across devices, with conflict-safe migrations, catalog tombstones, backup identity rotation, and atomic imports.

### Fixed

- Legacy cloud classification merging.

## [v1.2.0] - 2026-11-04

### Added

- Domain-routed memory management: decision-model-backed memory domains with JEV and Ollama support, scoped recall routing, domain tagging and filtering, classification workflows, and localized settings UI.
- Cross-device extracted-memory mirrors with deletion tombstones.

### Changed

- Improved L1 extraction recovery and cloud sync behavior.

### Fixed

- Frontend TypeScript errors in locales and Memory Center, control-character regex lint in graph label measurement, and missing session reader imports in Rust tests.

## [v1.1.1] - 2026-11-02

### Added

- DeepSeek Harness bridge integration: replaces the legacy dsh plugins with unified bridge installation and feature management, including memory injection, MCP observation, usage reporting, collection triggers, and native skill tools. Adds local HTTP bridge endpoints, idempotent telemetry ingestion, and immediate session collection.

### Changed

- Installer updates use direct Windows installer assets when available, open update URLs through the system browser, and display the compiled app version.
- README animations rendered as in-repo GIFs that GitHub can display.

### Fixed

- Raw ETags preserved for conditional writes: If-Match headers now use the original server-provided form, preventing repeated 412 failures during sync.

## [v1.1.0] - 2026-10-24

The refocus release — the app becomes one workspace for memory, Skills, and MCP.

### Added

- DeepSeek Harness as an agent source: zstd-compressed sessions are decoded in place during transcript scanning.
- Local semantic search settings and backend.
- Model registry for managing model connections; equipment profiles for reusing setups.
- Navigation search, in-app help documents, and window close-behavior settings.
- Rewritten MCP Library with configuration arbitration, import, and equip dialogs.

### Changed

- Telemetry store modularized into schema, transcript, and test modules.
- MCP sidecar binaries are built through the shared launcher script in CI and releases.
- Default theme changed from dark to light.

### Removed

- Agent process management: launch/monitor, streaming logs, embedded Web UI tabs, interactive terminal, and GitHub repository install.
- Proxy publishing and sharing: Cloudflare Tunnel temporary links and Caddy reverse proxy hosting.

## [v1.0.0] - 2026-10-09

The 1.0 release; this tag aggregates the internal v1.0.0–v1.0.2 series.

### Added

- MCP Library: a central registry of MCP servers across agents, with equip/unequip, drift status, and import from agent clients (8+1 targets).
- ZCode (Claude Code-compatible engine) agent source: transcript scanning with multi-account mirror directories and cross-source session dedup, a skill directory, and the shared-memory MCP.
- Cloud backup Vault service (axum + SQLite, token-hash authentication) with desktop cloud sync and backup restore over WebDAV/S3.
- Environment Variables page: user/system registry editing, a PATH entry editor, REG_EXPAND_SZ expansion preview, WM_SETTINGCHANGE broadcast, and deletion protection for critical variables.
- Scheduled L2/L3 background recomputation (defaults: L2 every 7 days, L3 every 30 days, configurable; L3 auto-publishes after rebuild).
- First-run onboarding wizard, sidebar grouping with expandable subpages, and Memory Center breadcrumbs.
- Cancellable manual memory extraction with running/cancelling states and interrupted-result messaging.
- 88API as the built-in sponsored model provider, with automatic model list fetching.
- Startup arguments: `--page`, `--lang`, `--select`, `--start`, `--open-ui`.

### Changed

- Renamed Agent Manager to Vibe Assistant (display name, prompts, docs, window title; protocol identifiers and data directories are preserved).
- Skill Center performance: 13 synchronous commands made async, publish-page drift checks moved off the main thread, React memoization and keep-mounted rendering.
- Built-in model presets consolidated to the 88API entry; old built-in records and stale memory-extraction selections are cleaned up on upgrade.
- README rewritten in English and Chinese with English as the default language, embedded feature screenshots, and redrawn bilingual Mermaid architecture diagrams.
- Page reorganization: Dashboard, Manager Agent, and McpAgent pages removed; MCP management merged into the MCP Library.
- Windows console windows hidden across 20+ process spawn points (CREATE_NO_WINDOW).

### Fixed

- Memory Center white-screen regression (hook ordering).
- Cloud full pull, cloud merge, and Memory Center sync issues; MiniMax region authentication and organize timeout; saved-PAT connection tests.
- L1 organize parsing hardened: truncated JSON salvaged element by element, singular `memory` keys, errors carry the model's original text.
- Settings license display corrected to Apache 2.0; productName and updater repository unified.
- macOS Intel release runner; Linux CI test fixtures reorganized by platform separator.

### Removed

- Workflow feature (frontend pages and backend modules).

## [v1.0.0_beta] - 2026-08-21

### Added

- Memory Center: cross-agent layered memory (L1 retrievable memories, L2 30-day working memory, L3 long-term Profile) automatically extracted from local coding-agent conversations, with custom memory editing and semantic cleaning and deduplication.
- Dual-channel memory injection: session-start injection (L3 + L2) for hook-capable agents, and a shared-memory MCP with on-demand semantic recall.
- Skill library: SKILL.md files scanned across agents into a shared library, with content-hash previews of additions, updates, and conflicts.
- Token usage aggregation from local transcripts aligned with provider billing, plus telemetry endpoints backed by a local SQLite event ledger.
- Agent sources infrastructure and a reworked LLM settings page.

## [v0.3.0] - 2026-07-12

### Added

- Workflow engine: visual workflow builder, live run view, runs history, a scheduler for timed runs, and external triggers.
- Data-analysis workflows: data-structure recognition, SQL generation and execution, chart recommendation and generation, business insight analysis, and Excel/Word/PPT report export.
- HTTP endpoint for external access to agents.

### Changed

- MCP agent and service module improvements.

## [v0.2.3] - 2026-06-22

Initial public release of 智管-Agent Manager.

### Added

- Unified management for local AI agents: one-click start/stop with streaming logs, PID/port status, embedded Web UI tabs, and an interactive terminal.
- Manager Agent: natural-language control of all agents.
- MCP Agent tool conversations.
- One-click temporary public share links (Cloudflare Tunnel) and Caddy reverse-proxy publishing for long-term hosting.
- Port Manager.
- CI and a manual, tag-driven release workflow bundling all platforms; in-app update checker and auto-updater.
- Resizable panels, logs, and actions.

[v1.3.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.3.0...v1.3.1
[v1.3.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.4...v1.3.0
[v1.2.4]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.3...v1.2.4
[v1.2.3]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.2...v1.2.3
[v1.2.2]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.1...v1.2.2
[v1.2.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.2.0...v1.2.1
[v1.2.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.1.1...v1.2.0
[v1.1.1]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.1.0...v1.1.1
[v1.1.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.0.0...v1.1.0
[v1.0.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v1.0.0_beta...v1.0.0
[v1.0.0_beta]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v0.3.0...v1.0.0_beta
[v0.3.0]: https://github.com/Zafer-Liu/Vibe_Assistant/compare/v0.2.3...v0.3.0
[v0.2.3]: https://github.com/Zafer-Liu/Vibe_Assistant/releases/tag/v0.2.3
