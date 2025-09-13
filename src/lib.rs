//! A custom logging layer for sending logs to Google Cloud Logging
//! using the `tracing` and `tracing-subscriber` crates.
//! This layer captures log events, enriches them with metadata
//! about the running environment, and sends them to Google Cloud Logging.
//! (C) 2025 Enzo Lombardi
use chrono::Local;
use gcp_auth::AuthenticationManager;

use serde::Serialize;
use std::time::Duration;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Metadata for a container, capturing its ID and name
#[derive(Debug, Serialize)]
pub struct ContainerMetadata {
    /// Unique identifier of the container
    pub id: String,
    /// Name of the container
    pub name: String,
}

/// Metadata for a Google Compute Engine instance
#[derive(Debug, Serialize)]
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
#[derive(Debug, Serialize)]
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

/// Custom logging layer for sending logs to Google Cloud Logging
pub struct GcpLoggingLayer {
    /// Channel for sending log entries
    channel: UnboundedSender<GcpLogEntry>,
}

impl GcpLoggingLayer {
    /// Create a new GcpLoggingLayer with authentication and log metadata
    ///
    /// # Arguments
    /// * `project_id` - The Google Cloud project ID
    ///
    /// # Returns
    /// A Result containing the initialized GcpLoggingLayer or an error
    pub async fn new(project_id: String) -> Result<Self, Box<dyn std::error::Error>> {
        // Try to initialize authentication, but allow initialization to succeed
        // even if authentication is not available (e.g. in tests or local dev).
        // In such cases we proceed with an empty token and continue sending logs
        // best-effort (requests will be unauthenticated).
        let token = match AuthenticationManager::new().await {
            Ok(auth) => match auth
                .get_token(&["https://www.googleapis.com/auth/logging.write"])
                .await
            {
                // Convert the acquired Token to a String representation so `token`
                // has a consistent `String` type across all match arms.
                Ok(tok) => tok.as_str().to_string(),
                Err(e) => {
                    eprintln!(
                        "Warning: failed to acquire GCP token: {}. Proceeding without auth.",
                        e
                    );
                    String::new()
                }
            },
            Err(e) => {
                eprintln!("Warning: failed to initialize AuthenticationManager: {}. Proceeding without auth.", e);
                String::new()
            }
        };

        // Attempt to collect metadata, but fall back to sensible defaults on error.
        // We clone project_id to allow creating a fallback instance that still
        // contains the provided project id in case metadata lookup fails.
        let metadata = match collect_log_metadata(project_id.clone()).await {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "Warning: failed to collect metadata: {}. Using fallback values.",
                    e
                );
                LogContextMetadata {
                    container: None,
                    instance: InstanceMetadata {
                        name: "unknown".into(),
                        id: "0".into(),
                        zone: "".into(),
                        project_id,
                    },
                }
            }
        };

        let (channel, mut rx) = unbounded_channel::<GcpLogEntry>();
        let client = reqwest::Client::new();

        // Spawn the background task that drains the channel and sends logs.
        // If we don't have a token (empty string), skip sending entries to avoid noisy errors.
        tokio::spawn(async move {
            // If token is empty we will not attempt HTTP requests; warn once.
            let mut warned_no_auth = false;
            let skip_sending = token.is_empty();

            loop {
                let log_entry = rx.recv().await;
                if log_entry.is_none() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    continue;
                }
                let log_entry = log_entry.unwrap();

                if skip_sending {
                    if !warned_no_auth {
                        eprintln!("Warning: no GCP auth token available; log entries will not be sent. Set up authentication to enable sending.");
                        warned_no_auth = true;
                    }
                    // Drop the entry without attempting to send it.
                    continue;
                }

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

                // Build the request and conditionally add auth if available.
                let mut req = client
                    .post("https://logging.googleapis.com/v2/entries:write")
                    .json(&body);
                if !token.is_empty() {
                    req = req.bearer_auth(token.as_str());
                }

                let res = req.send().await;
                if let Err(e) = res {
                    eprintln!("Failed to send log entry: {}", e);
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

        let result = self.channel.send(log_entry);
        if result.is_err() {
            eprintln!("Error {:?}", result);
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
pub async fn collect_log_metadata(
    project_id: String,
) -> Result<LogContextMetadata, Box<dyn std::error::Error>> {
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
