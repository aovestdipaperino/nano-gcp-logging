# Changelog

All notable changes to this project will be documented in this file.

The format is based on "Keep a Changelog" and this project adheres to
semantic versioning: https://semver.org/

## [Unreleased]

### Added
- Improve README with clearer Quickstart, Project ID detection, local development notes, and examples.
- Allow `GcpLoggingLayer::new(project_id: String)` to initialize even when GCP authentication or metadata are unavailable; emits warnings instead of failing.
- Add behavior to skip sending HTTP log entries when no auth token is present to avoid noisy network errors during local runs.
- Add example in `examples/basic.rs` demonstrating basic initialization and usage.
- Add tests exercising initialization and metadata collection.

### Changed
- Background sender now conditionally attaches a bearer token only when authentication is available.
- Metadata collection falls back to safe defaults when metadata server calls fail.
- Token handling normalized to `String` internally to simplify background task logic and error handling.
- README reorganized and expanded with development tips, behavior notes, and local testing instructions.

### Fixed
- Fix failing initialization in test environment by tolerating missing authentication and metadata.
- Prevent repeated noisy failure messages when running examples locally without credentials.

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

### Changed
- N/A (initial release)

### Fixed
- N/A (initial release)

---

Notes
- Future releases will include options for configurable buffering, retry/backoff strategies, and more robust testing hooks (e.g., injectable HTTP client for deterministic offline tests).
- If you depend on strict fail-fast behavior (i.e., require initialization to fail when auth/metadata are missing), consider validating the environment before calling `GcpLoggingLayer::new` or requesting a configuration option to enable strict mode.
