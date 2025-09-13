# nano-gcp-logging

[![Crates.io](https://img.shields.io/crates/v/nano-gcp-logging.svg)](https://crates.io/crates/nano-gcp-logging)
[![Docs.rs](https://docs.rs/nano-gcp-logging/badge.svg)](https://docs.rs/nano-gcp-logging)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

A compact tracing `Layer` that captures structured logs and ships them to Google Cloud Logging (Stackdriver). It enriches events with instance and container metadata when available and sends JSON payloads to the Logging API.

(C) 2025 Enzo Lombardi <enzinol@gmail.com>

---

Quick links
- Crates.io: https://crates.io/crates/nano-gcp-logging
- Docs: https://docs.rs/nano-gcp-logging
- Repository: https://github.com/aovestdipaperino/nano-gcp-logging

Features
- Structured log shipping to Google Cloud Logging
- Automatic enrichment with GCE/GKE metadata (when running on GCE/GKE)
- Async, non-blocking background sender using Tokio
- Integrates with the `tracing` / `tracing-subscriber` ecosystem
- Resilient local behavior: tolerates missing auth/metadata for local dev and tests

Table of contents
- Quickstart
- Project ID and metadata
- Usage example
- Behavior and local development notes
- Configuration & behavior choices
- Examples & tests
- Contributing
- License and author

Quickstart

1. Add to your `Cargo.toml`:
    nano-gcp-logging = "0.1"

2. Create and initialize the layer, then attach it to a `tracing` subscriber.

Project ID and metadata

- On a Google Cloud VM or GKE node you can query the metadata server to obtain the current project id:

    PROJECT_ID=$(curl -s -H "Metadata-Flavor: Google" http://metadata.google.internal/computeMetadata/v1/project/project-id)
    echo $PROJECT_ID

- In environments without access to the metadata server (local dev, CI), export `PROJECT_ID` in your environment before starting your application:

    export PROJECT_ID="my-project-id"

- The crate attempts to auto-discover instance and container metadata by querying the GCE metadata server. If that fails, the library falls back to reasonable defaults (so your app can run locally or in CI without failing on startup).

Usage example

Below is a minimal example demonstrating initialization and attaching the layer to a tracing subscriber. Replace `your-gcp-project-id` with your project id or set the `PROJECT_ID` env var.

    use nano_gcp_logging::GcpLoggingLayer;
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;
    use std::env;

    #[tokio::main]
    async fn main() -> Result<(), Box<dyn std::error::Error>> {
        let project_id = env::var("PROJECT_ID").unwrap_or_else(|_| "your-gcp-project-id".into());
        let gcp_layer = GcpLoggingLayer::new(project_id).await?;

        let subscriber = Registry::default().with(gcp_layer);
        tracing::subscriber::set_global_default(subscriber)?;

        tracing::info!("Hello from nano-gcp-logging!");
        Ok(())
    }

Behavior and local development notes

- Authentication
  - When running on GCE/GKE, the library will attempt to use the application's default credentials (via `gcp_auth`) to obtain an OAuth token with the `logging.write` scope.
  - When authentication is not available (local dev / CI without credentials), the layer will still initialize but will not attempt to send HTTP requests. A single warning is emitted to indicate that log shipping is disabled until auth is configured.
  - This makes it easier to run examples and tests locally without requiring real credentials.

- Metadata discovery
  - The layer queries the metadata server for instance name, id, zone, and container id (if present in cgroups).
  - If metadata queries fail, the library will fall back to defaults and keep working. The provided `project_id` (from env or constructor) is preserved.

- Background sending
  - The layer uses an unbounded channel to avoid blocking the main tracing fast-path. A background task drains the channel and sends batched JSON entries to the Logging API.
  - When auth is missing the background task is a no-op (drops entries) to avoid noisy network errors; when auth is present entries are sent with the appropriate bearer token.

Examples & tests

- `examples/basic.rs` demonstrates basic initialization and emits a few log events. For local runs, setting `PROJECT_ID` is recommended.
- Integration tests live in `tests/`. The crate is designed so tests can run without real GCP credentials — initialized layers will warn and use fallbacks.

Local testing tips

- To run tests locally without GCP credentials:

    export PROJECT_ID="local-project"
    export HOSTNAME="$(hostname)"
    cargo test

- To run the example locally:

    export PROJECT_ID="local-project"
    cargo run --example basic

- To test sending logs to GCP in a development environment:
  - Configure ADC (Application Default Credentials) locally, or run on a VM/GKE node with the proper service account and logging permissions.
  - Confirm your service account has `roles/logging.logWriter` or equivalent.

Design notes & rationale

- Fail-fast vs. permissive initialization
  - The library takes a permissive approach: initialization succeeds even if metadata or authentication are not available. This reduces friction during development and when running unit/integration tests that don't interact with GCP.
  - If you prefer fail-fast behavior (i.e., panic or return an error when auth/metadata are missing), consider wrapping initialization in your own helper that validates the environment and returns an error when requirements are unmet.

- Back-pressure and buffering
  - Currently the layer uses an unbounded channel to minimize overhead in the tracing hot path. Depending on your application's throughput and reliability requirements you might want a bounded queue with retry/backoff and durable local buffering.

Configuration & extension points

- `GcpLoggingLayer::new(project_id: String)` — construct the layer with an explicit project id.
- You can extend the layer to:
  - Provide custom HTTP client with retries and exponential backoff.
  - Add local durable buffering (file-backed queue) for high-reliability scenarios.
  - Toggle behavior on missing auth (drop vs. buffer vs. fail).

Contributing

- Contributions, bug reports and PRs are welcome. Please follow the repository's contribution guidelines and coding style.
- Run tests with:

    cargo test

License

- Licensed under the Apache License, Version 2.0.

Author

- Enzo Lombardi — <enzinol@gmail.com>
