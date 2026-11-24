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
use tokio::sync::{mpsc, oneshot};
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

/// What travels from the tracing layer to the background sender.
enum Command {
    /// A log entry to deliver.
    Entry(GcpLogEntry),
    /// Acknowledge once every entry queued before this command has been sent.
    Flush(oneshot::Sender<()>),
}

/// Default log name, kept from earlier releases so existing logs stay where they are.
const DEFAULT_LOG_NAME: &str = "proxie";
/// Default Cloud Logging write endpoint.
const DEFAULT_ENDPOINT: &str = "https://logging.googleapis.com/v2/entries:write";
/// How long to wait for the GCE metadata server before deciding we are not on GCE.
const DEFAULT_METADATA_TIMEOUT: Duration = Duration::from_millis(750);
/// How long a single delivery attempt may take.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Targets whose events are never shipped. The layer sends logs over HTTP, and
/// the HTTP stack emits `tracing` events of its own; shipping those would make
/// every delivery generate more entries to deliver, without end.
const IGNORED_TARGET_PREFIXES: &[&str] = &[
    "hyper",
    "hyper_util",
    "reqwest",
    "h2",
    "rustls",
    "tokio_rustls",
    "tower",
    "want",
    "gcp_auth",
    "nano_gcp_logging",
];

fn is_ignored_target(target: &str) -> bool {
    IGNORED_TARGET_PREFIXES.iter().any(|prefix| {
        target == *prefix
            || target
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

/// Custom logging layer for sending logs to Google Cloud Logging
///
/// Cloning the layer is cheap and every clone feeds the same background
/// sender, so keep one clone to call [`flush`](Self::flush) on after another
/// has been handed to the subscriber.
#[derive(Clone)]
pub struct GcpLoggingLayer {
    /// Channel for sending log entries
    channel: mpsc::Sender<Command>,
}

/// Configures a [`GcpLoggingLayer`].
///
/// ```no_run
/// # async fn demo() -> Result<(), nano_gcp_logging::GcpLoggingError> {
/// let layer = nano_gcp_logging::GcpLoggingLayer::builder("my-project")
///     .log_name("my-app")
///     .skip_metadata() // not on Google Cloud: do not probe the metadata server
///     .build()
///     .await?;
/// # Ok(()) }
/// ```
pub struct GcpLoggingLayerBuilder {
    project_id: String,
    log_name: String,
    endpoint: String,
    skip_metadata: bool,
    metadata_timeout: Duration,
    token_provider: Option<Arc<dyn TokenProvider>>,
}

impl GcpLoggingLayerBuilder {
    /// The Cloud Logging log the entries are written to (default `"proxie"`).
    pub fn log_name(mut self, log_name: impl Into<String>) -> Self {
        self.log_name = log_name.into();
        self
    }

    /// Do not query the GCE metadata server. Use this when you know the program
    /// is not running on Google Cloud: entries are written against the `global`
    /// resource and startup does not wait for the metadata server at all.
    pub fn skip_metadata(mut self) -> Self {
        self.skip_metadata = true;
        self
    }

    /// How long to wait for the metadata server before assuming the program is
    /// not running on Google Cloud (default 750 ms).
    pub fn metadata_timeout(mut self, timeout: Duration) -> Self {
        self.metadata_timeout = timeout;
        self
    }

    /// Use this token provider instead of discovering credentials with `gcp_auth`.
    pub fn token_provider(mut self, provider: Arc<dyn TokenProvider>) -> Self {
        self.token_provider = Some(provider);
        self
    }

    /// Write to a different `entries:write` endpoint, for example an emulator.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Authenticate, collect metadata and start the background sender.
    ///
    /// # Errors
    /// Returns `GcpLoggingError::Auth` if no credentials can be found or a token
    /// cannot be obtained.
    pub async fn build(self) -> Result<GcpLoggingLayer, GcpLoggingError> {
        let provider = match self.token_provider {
            Some(provider) => provider,
            None => gcp_auth::provider().await?,
        };

        // Verify we can get a token at startup
        let _token = provider.token(GcpLoggingLayer::SCOPES).await?;
        let metadata = if self.skip_metadata {
            offline_metadata(self.project_id)
        } else {
            collect_log_metadata_with_timeout(self.project_id, self.metadata_timeout).await?
        };
        let (channel, mut rx) = mpsc::channel::<Command>(LOG_CHANNEL_CAPACITY);
        let client = reqwest::Client::builder().timeout(SEND_TIMEOUT).build()?;
        let config = Arc::new(SenderConfig {
            log_name: self.log_name,
            endpoint: self.endpoint,
        });

        tokio::spawn(async move {
            loop {
                let result = std::panic::AssertUnwindSafe(gcp_log_sender(
                    provider.clone(),
                    metadata.clone(),
                    client.clone(),
                    config.clone(),
                    &mut rx,
                ));
                if futures::FutureExt::catch_unwind(result).await.is_err() {
                    eprintln!("GCP log sender panicked, restarting in 5s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        });

        Ok(GcpLoggingLayer { channel })
    }
}

/// Settings the background sender needs on every request.
struct SenderConfig {
    log_name: String,
    endpoint: String,
}

impl GcpLoggingLayer {
    /// GCP logging scopes
    const SCOPES: &[&str] = &["https://www.googleapis.com/auth/logging.write"];

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
    pub async fn new(project_id: String) -> Result<Self, GcpLoggingError> {
        Self::builder(project_id).build().await
    }

    /// Start configuring a layer: log name, metadata detection, credentials.
    pub fn builder(project_id: impl Into<String>) -> GcpLoggingLayerBuilder {
        GcpLoggingLayerBuilder {
            project_id: project_id.into(),
            log_name: DEFAULT_LOG_NAME.to_string(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            skip_metadata: false,
            metadata_timeout: DEFAULT_METADATA_TIMEOUT,
            token_provider: None,
        }
    }

    /// Wait until every entry logged so far has been sent to Cloud Logging.
    ///
    /// Flush through a clone of the layer, the one you did not give to the subscriber.
    ///
    /// Entries are delivered by a background task, so a program that returns
    /// from `main` right after logging can exit before they leave the process.
    /// Call this before exiting. It waits as long as delivery takes; wrap it in
    /// `tokio::time::timeout` if you need an upper bound.
    pub async fn flush(&self) {
        let (ack, done) = oneshot::channel();
        if self.channel.send(Command::Flush(ack)).await.is_ok() {
            let _ = done.await;
        }
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
        if is_ignored_target(event.metadata().target()) {
            return;
        }

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

        let result = self.channel.try_send(Command::Entry(log_entry));
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
    config: Arc<SenderConfig>,
    rx: &mut mpsc::Receiver<Command>,
) {
    loop {
        let Some(command) = rx.recv().await else {
            // Channel closed — sleep briefly to avoid busy loop on spurious wakeups
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let log_entry = match command {
            Command::Entry(entry) => entry,
            Command::Flush(ack) => {
                // Everything queued before this command has been handled.
                let _ = ack.send(());
                continue;
            }
        };

        // Get a fresh (or cached) token for each request
        let token = match provider.token(GcpLoggingLayer::SCOPES).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("GCP auth token refresh failed: {}", e);
                continue;
            }
        };

        let body = serde_json::json!({
            "entries": [build_entry(&config.log_name, &metadata, &log_entry)]
        });
        let res = client
            .post(&config.endpoint)
            .bearer_auth(token.as_str())
            .json(&body)
            .send()
            .await;
        match res {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                // Cloud Logging answers a rejected entry with an HTTP error, not a
                // transport failure; without this check it would vanish silently.
                let status = response.status();
                let detail = response.text().await.unwrap_or_default();
                eprintln!(
                    "Cloud Logging rejected a log entry: HTTP {}: {}",
                    status,
                    detail.trim()
                );
            }
            Err(e) => eprintln!("Failed to send log entry: {}", error_chain(&e)),
        }
    }
}

/// Build one `LogEntry`. On a GCE instance the entry is tied to the instance;
/// anywhere else it uses the `global` resource, because a `gce_instance`
/// resource with empty labels is not accepted.
fn build_entry(
    log_name: &str,
    metadata: &LogContextMetadata,
    log_entry: &GcpLogEntry,
) -> serde_json::Value {
    let on_gce = !metadata.instance.id.is_empty() && !metadata.instance.zone.is_empty();
    let resource = if on_gce {
        serde_json::json!({
            "type": "gce_instance",
            "labels": {
                "instance_id": metadata.instance.id,
                "zone": metadata.instance.zone,
                "project_id": metadata.instance.project_id
            }
        })
    } else {
        serde_json::json!({
            "type": "global",
            "labels": { "project_id": metadata.instance.project_id }
        })
    };
    serde_json::json!({
        "logName": format!("projects/{}/logs/{}", metadata.instance.project_id, log_name),
        "resource": resource,
        "severity": log_entry.severity,
        "jsonPayload": {
            "message": log_entry.message,
            "container": metadata.container,
            "instance": metadata.instance
        }
    })
}

/// An error with its causes: `reqwest` hides the useful part (DNS failure,
/// refused connection, bad certificate) behind "error sending request".
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
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
    collect_log_metadata_with_timeout(project_id, DEFAULT_METADATA_TIMEOUT).await
}

/// Metadata for a program that is not on Google Cloud: nothing is queried.
fn offline_metadata(project_id: String) -> LogContextMetadata {
    LogContextMetadata {
        container: None,
        instance: InstanceMetadata {
            name: String::new(),
            id: String::new(),
            zone: String::new(),
            project_id,
        },
    }
}

async fn collect_log_metadata_with_timeout(
    project_id: String,
    timeout: Duration,
) -> Result<LogContextMetadata, GcpLoggingError> {
    let client = reqwest::Client::builder().timeout(timeout).build()?;

    let container_id = get_container_id();
    let container_name = std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into());

    let container_metadata = container_id.clone().map(|id| ContainerMetadata {
        id,
        name: container_name,
    });

    // One probe decides whether this is Google Cloud at all: when the metadata
    // server does not answer, skip the other lookups instead of waiting for
    // each of them to time out.
    let (instance_name, instance_id, zone_path) = match get_metadata(&client, "instance/id").await {
        Some(id) => (
            get_metadata(&client, "instance/name")
                .await
                .unwrap_or_default(),
            id,
            get_metadata(&client, "instance/zone")
                .await
                .unwrap_or_default(),
        ),
        None => Default::default(),
    };
    let zone = zone_path.split('/').next_back().unwrap_or("").to_string();

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
        .find_map(|line| line.rfind('/').map(|pos| line[pos + 1..].to_string()))
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
        .error_for_status()
        .ok()?
        .text()
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> GcpLogEntry {
        GcpLogEntry {
            message: "m".into(),
            severity: "INFO".into(),
        }
    }

    fn metadata(id: &str, zone: &str) -> LogContextMetadata {
        LogContextMetadata {
            container: None,
            instance: InstanceMetadata {
                name: "vm".into(),
                id: id.into(),
                zone: zone.into(),
                project_id: "p".into(),
            },
        }
    }

    #[test]
    fn http_stack_targets_are_ignored_but_user_targets_are_not() {
        assert!(is_ignored_target("hyper"));
        assert!(is_ignored_target("hyper_util::client::legacy::pool"));
        assert!(is_ignored_target("reqwest::connect"));
        assert!(is_ignored_target("nano_gcp_logging"));
        assert!(!is_ignored_target("hyperion"));
        assert!(!is_ignored_target("my_app::hyper_wrapper"));
        assert!(!is_ignored_target("my_app"));
    }

    #[test]
    fn a_gce_instance_is_logged_as_gce_instance() {
        let e = build_entry("app", &metadata("123", "europe-west1-b"), &entry());
        assert_eq!(e["resource"]["type"], "gce_instance");
        assert_eq!(e["resource"]["labels"]["zone"], "europe-west1-b");
        assert_eq!(e["logName"], "projects/p/logs/app");
    }

    #[test]
    fn missing_instance_metadata_falls_back_to_the_global_resource() {
        let e = build_entry("app", &metadata("", ""), &entry());
        assert_eq!(e["resource"]["type"], "global");
        assert_eq!(e["resource"]["labels"]["project_id"], "p");
    }
}
