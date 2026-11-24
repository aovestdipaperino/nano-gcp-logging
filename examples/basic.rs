// A basic example demonstrating how to use the nano-gcp-logging crate
// to send logs to Google Cloud Logging.
use nano_gcp_logging::GcpLoggingLayer;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, Registry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Replace with your actual GCP project ID
    let project_id = "your-gcp-project-id".to_string();

    // Initialize the GCP logging layer
    let gcp_layer = GcpLoggingLayer::new(project_id).await?;

    // Keep a clone to flush with: logs are sent by a background task, and the
    // program would otherwise exit before they leave the process.
    let flusher = gcp_layer.clone();

    // Set up the tracing subscriber with the GCP logging layer
    let subscriber = Registry::default().with(gcp_layer);
    tracing::subscriber::set_global_default(subscriber)?;

    // Emit some log events
    info!("Hello from nano-gcp-logging!");
    warn!("This is a warning example.");
    error!("This is an error example.");

    // Wait until everything logged above has been sent
    flusher.flush().await;

    Ok(())
}
