# Changelog

All notable changes to this project will be documented in this file.

The format is based on "Keep a Changelog" and this project adheres to
semantic versioning: https://semver.org/

## [0.2.0] - 2026-03-21

### Fixed
- **CRITICAL: Fix segfault in token refresh** — upgrade `gcp_auth` from 0.10 to 0.12, replacing deprecated `AuthenticationManager` with `TokenProvider` API. The old version caused a null pointer dereference in the token refresh closure after ~19 minutes of operation.
- **CRITICAL: Fix token expiration after ~1 hour** — tokens are now refreshed on every send via `provider.token()` (the provider caches internally and only refreshes when expired). Previously, a single token was acquired at startup and used forever, causing silent auth failures after expiry.
- **Add panic guard around log sender task** — the background sender loop is now wrapped in `catch_unwind` with automatic restart after 5 seconds. Previously, a panic in the sender would silently kill the logging task.

### Changed
- **BREAKING: `new()` returns `Result<Self, GcpLoggingError>` instead of `Result<Self, Box<dyn Error>>`** — typed error enum for better error handling.
- Switch from unbounded channel to bounded `mpsc::channel(8192)` with backpressure — prevents unbounded memory growth under high log volume.
- Use `try_send` instead of `send` on the bounded channel — non-blocking on the logging hot path.
- Structs `ContainerMetadata`, `InstanceMetadata`, `LogContextMetadata` now derive `Clone`.
- Added `futures` and `thiserror` as dependencies.

---

## [0.1.0] - 2025-09-13

### Added
- Initial release of `nano-gcp-logging`.
  - `GcpLoggingLayer` tracing layer for sending structured logs to Google Cloud Logging.
  - Automatic severity mapping from `tracing` levels to Cloud Logging severities.
  - Instance and container metadata enrichment (when running on GCE/GKE).
  - Async background log delivery using Tokio and `reqwest`.
  - `collect_log_metadata` helper for gathering instance/container metadata.
  - Basic example and integration test(s).
