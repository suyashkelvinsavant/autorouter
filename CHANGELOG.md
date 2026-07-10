# Changelog

All notable changes to AutoRouter are documented here.

## [0.1.1] - 2026-07-11

### Fixed

- Restored a clean strict-Clippy CI baseline across the workspace.
- Simplified streaming and session-support code without changing the public gateway behavior.

## [0.1.0] - 2026-07-10

### Added

- Local-first desktop gateway that accepts OpenAI, Anthropic, and Gemini wire formats.
- Universal request/response translation pipeline with streaming and tool-use support.
- Smart routing with rules, capability matching, and per-model health tracking.
- Desktop dashboard, headless gateway, persistent configuration, and secret-store integration.
- Windows MSI and NSIS installer builds, plus CI builds for macOS and Linux.

### Security

- Optional bearer authentication for every gateway and dashboard route.
- API keys are resolved from environment variables or the platform secret store; they are not committed to configuration.

[0.1.1]: ../../releases/tag/v0.1.1
[0.1.0]: ../../releases/tag/v0.1.0
