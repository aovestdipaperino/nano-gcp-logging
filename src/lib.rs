//! A custom logging layer for sending logs to Google Cloud Logging
//! using the `tracing` and `tracing-subscriber` crates.
//! This layer captures log events, enriches them with metadata
//! about the running environment, and sends them to Google Cloud Logging.
//! (C) 2025 Enzo Lombardi

use chrono::Local;
use gcp_auth::TokenProvider;

use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Maximum number of log entries that can be buffered before backpressure.
/// Sized to absorb bursts without dropping logs under normal operation.
const LOG_CHANNEL_CAPACITY: usize = 8192;

/// Metadata for a container, capturing its ID and name
#[derive(Debug, Clone, Serialize)]
pub struct ContainerMetadata {
    /// Unique identifier of the container
    pub id: String,
    /// Name of the container
    pub name: String,
}

/// Metadata for a Google Compute Engine instance
#[derive(Debug, Clone, Serialize)]
pub struct InstanceMetadata {
    /// Name of the instance
    pub name: String,
    /// Unique identifier of the instance
    pub id: String,
    /// Zone where the instance is located
    pub zone: String,
    /// Google Cloud project ID
    pub project_id: String,
}

/// Comprehensive logging context metadata
#[derive(Debug, Clone, Serialize)]
pub struct LogContextMetadata {
    /// Optional container metadata
    pub container: Option<ContainerMetadata>,
    /// Instance metadata
    pub instance: InstanceMetadata,
}

/// Structured log entry for Google Cloud Logging
#[derive(Debug, Serialize)]
struct GcpLogEntry {
    /// Log message content
    message: String,
    /// Severity level of the log entry
    severity: String,
}

/// Errors that can occur during GCP logging initialization and operation.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum GcpLoggingError {
    #[error("GCP authentication failed: {0}")]
    Auth(#[from] gcp_auth::Error),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
}

/// Custom logging layer for sending logs to Google Cloud Logging
pub struct GcpLoggingLayer {
    /// Channel for sending log entries
    channel: mpsc::Sender<GcpLogEntry>,
}

impl GcpLoggingLayer {
    /// Create a new GcpLoggingLayer with authentication and log metadata
    ///
    /// # Arguments
    /// * `project_id` - The Google Cloud project ID
    ///
    /// # Returns
    /// A Result containing the initialized GcpLoggingLayer or an error
    ///
    /// # Errors
    /// Returns `GcpLoggingError::Auth` if GCP authentication fails.
    /// Returns `GcpLoggingError::Http` if metadata collection fails.

    /// GCP logging scopes
    const SCOPES: &[&str] = &["https://www.googleapis.com/auth/logging.write"];

    pub async fn new(project_id: String) -> Result<Self, GcpLoggingError> {
        let provider = gcp_auth::provider().await?;

        // Verify we can get a token at startup
        let _token = provider.token(Self::SCOPES).await?;
        let metadata = collect_log_metadata(project_id).await?;
        let (channel, mut rx) = mpsc::channel::<GcpLogEntry>(LOG_CHANNEL_CAPACITY);
        let client = reqwest::Client::new();

        tokio::spawn(async move {
            loop {
                let result = std::panic::AssertUnwindSafe(
                    gcp_log_sender(provider.clone(), metadata.clone(), client.clone(), &mut rx),
                );
                if futures::FutureExt::catch_unwind(result).await.is_err() {
                    eprintln!("GCP log sender panicked, restarting in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });

        Ok(Self { channel })
    }

    /// Map tracing log level to Google Cloud Logging severity
    ///
    /// # Arguments
    /// * `level` - The tracing log level
    ///
    /// # Returns
    /// A static string representing the corresponding severity level
    fn map_level_to_severity(level: &tracing::Level) -> &'static str {
        match *level {
            tracing::Level::ERROR => "ERROR",
            tracing::Level::WARN => "WARNING",
            tracing::Level::INFO => "INFO",
            tracing::Level::DEBUG => "DEBUG",
            tracing::Level::TRACE => "DEBUG",
        }
    }
}

impl<S> Layer<S> for GcpLoggingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    /// Process log events and send them to Google Cloud Logging
    ///
    /// # Arguments
    /// * `event` - The log event to process
    /// * `_ctx` - The tracing context
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut message = "**UNDEFINED**".to_string();

        event.record(
            &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                if field.name() == "message" {
                    message = format!("{:?}", value);
                }
            },
        );
        let now = Local::now().format("%Y-%m-%d %H:%M:%S,%3f").to_string();

        let metadata = event.metadata();
        let severity = Self::map_level_to_severity(metadata.level()).to_string();
        let message = format!(
            "[{}] {} [{} {}:{}] [{}]",
            now,
            severity,
            metadata.target(),
            metadata.file().unwrap_or("unknown_file"),
            metadata.line().unwrap_or(0),
            message
        );

        let log_entry = GcpLogEntry { severity, message };

        let result = self.channel.try_send(log_entry);
        if let Err(e) = result {
            eprintln!("Failed to send log entry: {}", e);
        }
    }
}

/// Background log sender loop. Refreshes the token via the provider on each send
/// (the provider caches tokens internally and only refreshes when expired).
async fn gcp_log_sender(
    provider: Arc<dyn TokenProvider>,
    metadata: LogContextMetadata,
    client: reqwest::Client,
    rx: &mut mpsc::Receiver<GcpLogEntry>,
) {
    loop {
        let Some(log_entry) = rx.recv().await else {
            // Channel closed — sleep briefly to avoid busy loop on spurious wakeups
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };

        // Get a fresh (or cached) token for each request
        let token = match provider.token(GcpLoggingLayer::SCOPES).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("GCP auth token refresh failed: {}", e);
                continue;
            }
        };

        let entry = serde_json::json!({
            "logName": format!("projects/{}/logs/proxie", metadata.instance.project_id),
            "resource": {
                "type": "gce_instance",
                "labels": {
                    "instance_id": metadata.instance.id,
                    "zone": metadata.instance.zone,
                    "project_id": metadata.instance.project_id
                }
            },
            "severity": log_entry.severity,
            "jsonPayload": {
                "message": log_entry.message,
                "container": metadata.container,
                "instance": metadata.instance
            }
        });
        let body = serde_json::json!({ "entries": [entry] });
        let res = client
            .post("https://logging.googleapis.com/v2/entries:write")
            .bearer_auth(token.as_str())
            .json(&body)
            .send()
            .await;
        if let Err(e) = res {
            eprintln!("Failed to send log entry: {}", e);
        }
    }
}

/// Collect comprehensive log metadata for the current instance
///
/// # Arguments
/// * `project_id` - The Google Cloud project ID
///
/// # Returns
/// A Result containing the LogContextMetadata or an error
///
/// # Errors
/// Returns `GcpLoggingError::Http` if the metadata service is unreachable.
pub async fn collect_log_metadata(
    project_id: String,
) -> Result<LogContextMetadata, GcpLoggingError> {
    let client = reqwest::Client::new();

    let container_id = get_container_id();
    let container_name = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());

    let container_metadata = container_id.clone().map(|id| ContainerMetadata {
        id,
        name: container_name,
    });

    let instance_name = get_metadata(&client, "instance/name")
        .await
        .unwrap_or_default();
    let instance_id = get_metadata(&client, "instance/id")
        .await
        .unwrap_or_default();
    let zone_path = get_metadata(&client, "instance/zone")
        .await
        .unwrap_or_default();
    let zone = zone_path.split('/').last().unwrap_or("").to_string();

    Ok(LogContextMetadata {
        container: container_metadata,
        instance: InstanceMetadata {
            name: instance_name,
            id: instance_id,
            zone,
            project_id,
        },
    })
}

/// Retrieve the current container ID from cgroup
///
/// # Returns
/// An optional container ID string
fn get_container_id() -> Option<String> {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()?
        .lines()
        .find_map(|line| {
            if let Some(pos) = line.rfind('/') {
                Some(line[pos + 1..].to_string())
            } else {
                None
            }
        })
}

/// Retrieve metadata from Google Cloud metadata service
///
/// # Arguments
/// * `client` - A reqwest client
/// * `path` - The metadata path to retrieve
///
/// # Returns
/// An optional string containing the metadata value
async fn get_metadata(client: &reqwest::Client, path: &str) -> Option<String> {
    let url = format!(
        "http://metadata.google.internal/computeMetadata/v1/{}",
        path
    );
    client
        .get(&url)
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()
}
