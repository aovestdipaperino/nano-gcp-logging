// A basic test for the GcpLoggingLayer to ensure it initializes correctly
// and can handle log events without panicking.

use nano_gcp_logging::{collect_log_metadata, GcpLoggingLayer};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, Registry};

#[tokio::test]
async fn test_gcp_logging_layer_basic() {
    // Use a dummy project id for testing
    let project_id = "dummy-project-id".to_string();

    // Try to create the logging layer
    let layer = GcpLoggingLayer::new(project_id.clone()).await;
    assert!(layer.is_ok(), "Failed to create GcpLoggingLayer");

    let gcp_layer = layer.unwrap();

    // Set up a tracing subscriber with our layer
    let subscriber = Registry::default().with(gcp_layer);
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set global subscriber");

    // Emit some log events
    info!("This is an info log");
    warn!("This is a warning log");
    error!("This is an error log");

    // Collect metadata (should not fail)
    let metadata = collect_log_metadata(project_id).await;
    assert!(metadata.is_ok(), "Failed to collect log metadata");
}
