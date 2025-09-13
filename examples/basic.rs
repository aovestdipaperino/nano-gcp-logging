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

    // Set up the tracing subscriber with the GCP logging layer
    let subscriber = Registry::default().with(gcp_layer);
    tracing::subscriber::set_global_default(subscriber)?;

    // Emit some log events
    info!("Hello from nano-gcp-logging!");
    warn!("This is a warning example.");
    error!("This is an error example.");

    Ok(())
}
