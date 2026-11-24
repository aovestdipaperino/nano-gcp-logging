# Changelog

All notable changes to this project will be documented in this file.

The format is based on "Keep a Changelog" and this project adheres to
semantic versioning: https://semver.org/

## [Unreleased]

### Fixed
- **Logs were silently lost when a program exited right after logging.** Entries are delivered by a background task and nothing waited for it. Added `GcpLoggingLayer::flush()` (and made the layer `Clone` so one clone can flush while another is in the subscriber).
- **Runaway self-logging.** The layer shipped the `tracing` events of its own HTTP client (`hyper`, `reqwest`, `h2`, `rustls`, ...), and every delivery produced more of them. One `info!` could generate thousands of entries. Events from those targets are now skipped.
- **Rejected entries were invisible.** Only transport failures were reported; an HTTP 4xx from Cloud Logging (permission, invalid resource) was dropped without a word. The status and response body are now printed to stderr, and transport errors show their causes.
- **Entries off Google Cloud used a `gce_instance` resource with empty labels,** which describes no real instance and is likely to be rejected. They now use the `global` resource; `gce_instance` is used only when the instance id and zone are known.
- **Metadata lookups could slow or confuse startup.** The metadata requests had no timeout and an HTTP error page was taken as the value. They now time out (750 ms by default), stop after the first failed probe, and ignore error responses.

### Added
- `GcpLoggingLayer::builder(project_id)` with `log_name`, `skip_metadata`, `metadata_timeout`, `token_provider` and `endpoint`.
- `GcpLoggingLayer::flush()`.

### Changed
- README corrected: it described 0.1 behaviour (initialising without credentials, an unbounded channel) that has not applied since 0.2.0.

---

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
