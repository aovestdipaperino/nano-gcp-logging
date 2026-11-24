// Delivery tests against a local stand-in for the Cloud Logging API, with a
// fake token provider, so they need no Google Cloud credentials.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use gcp_auth::{Token, TokenProvider};
use nano_gcp_logging::GcpLoggingLayer;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

struct StaticToken;

#[async_trait]
impl TokenProvider for StaticToken {
    async fn token(&self, _scopes: &[&str]) -> Result<Arc<Token>, gcp_auth::Error> {
        let token: Token =
            serde_json::from_str(r#"{"access_token":"test-token","expires_in":3600}"#).unwrap();
        Ok(Arc::new(token))
    }

    async fn project_id(&self) -> Result<Arc<str>, gcp_auth::Error> {
        Ok(Arc::from("test-project"))
    }
}

/// What the stand-in server saw.
#[derive(Default)]
struct Seen {
    bodies: Vec<Value>,
    auth_headers: Vec<String>,
}

/// Start a server that answers every request with `status` and records it.
async fn mock_logging_api(status: u16) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v2/entries:write", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let recorded = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, content_length) = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while buf.len() < head_end + content_length {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let auth = head
                    .lines()
                    .find(|l| l.to_lowercase().starts_with("authorization:"))
                    .unwrap_or("")
                    .to_string();
                let body: Value = serde_json::from_slice(&buf[head_end..head_end + content_length])
                    .unwrap_or(Value::Null);
                {
                    let mut seen = recorded.lock().unwrap();
                    seen.bodies.push(body);
                    seen.auth_headers.push(auth);
                }
                let reply = if status == 200 {
                    "{}".to_string()
                } else {
                    r#"{"error":{"code":403,"message":"The caller does not have permission"}}"#
                        .to_string()
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (url, seen)
}

async fn layer(endpoint: &str) -> GcpLoggingLayer {
    {
        GcpLoggingLayer::builder("test-project")
            .log_name("test-log")
            .skip_metadata()
            .token_provider(Arc::new(StaticToken))
            .endpoint(endpoint)
            .build()
            .await
            .expect("layer builds")
    }
}

fn entries(seen: &Seen) -> Vec<&Value> {
    seen.bodies
        .iter()
        .flat_map(|b| b["entries"].as_array().into_iter().flatten())
        .collect()
}

#[tokio::test]
async fn flush_delivers_everything_logged_before_it() {
    let (url, seen) = mock_logging_api(200).await;
    let layer = layer(&url).await;
    let subscriber = Registry::default().with(layer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    info!("first");
    error!("second");
    layer.flush().await;

    let seen = seen.lock().unwrap();
    let all = entries(&seen);
    assert_eq!(
        all.len(),
        2,
        "both entries must have been sent when flush returns"
    );
    assert_eq!(all[0]["logName"], "projects/test-project/logs/test-log");
    assert_eq!(all[0]["severity"], "INFO");
    assert!(all[0]["jsonPayload"]["message"]
        .as_str()
        .unwrap()
        .contains("first"));
    assert_eq!(all[1]["severity"], "ERROR");
    assert_eq!(
        seen.auth_headers[0].to_lowercase(),
        "authorization: bearer test-token"
    );
}

#[tokio::test]
async fn outside_google_cloud_entries_use_the_global_resource() {
    let (url, seen) = mock_logging_api(200).await;
    let layer = layer(&url).await;
    let _guard = tracing::subscriber::set_default(Registry::default().with(layer.clone()));

    info!("hello");
    layer.flush().await;

    let seen = seen.lock().unwrap();
    let entry = entries(&seen)[0].clone();
    assert_eq!(entry["resource"]["type"], "global");
    assert_eq!(entry["resource"]["labels"]["project_id"], "test-project");
}

#[tokio::test]
async fn events_from_the_http_stack_are_not_shipped() {
    let (url, seen) = mock_logging_api(200).await;
    let layer = layer(&url).await;
    let _guard = tracing::subscriber::set_default(Registry::default().with(layer.clone()));

    tracing::trace!(target: "hyper_util::client::legacy::pool", "checkout dropped");
    tracing::debug!(target: "reqwest::connect", "starting new connection");
    info!("only this one");
    layer.flush().await;
    // Give any feedback loop time to show itself before counting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    layer.flush().await;

    let seen = seen.lock().unwrap();
    let all = entries(&seen);
    assert_eq!(all.len(), 1, "got: {all:?}");
    assert!(all[0]["jsonPayload"]["message"]
        .as_str()
        .unwrap()
        .contains("only this one"));
}

#[tokio::test]
async fn a_rejecting_server_does_not_hang_flush() {
    let (url, seen) = mock_logging_api(403).await;
    let layer = layer(&url).await;
    let _guard = tracing::subscriber::set_default(Registry::default().with(layer.clone()));

    info!("will be rejected");
    tokio::time::timeout(Duration::from_secs(5), layer.flush())
        .await
        .expect("flush returns even when Cloud Logging rejects the entry");

    assert_eq!(entries(&seen.lock().unwrap()).len(), 1);
}
