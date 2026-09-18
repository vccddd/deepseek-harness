//! Host plugins-events bridge.
//!
//! WKWebView custom protocols answer with one complete body, so the page
//! cannot receive the Host's `/plugins/events` Server-Sent Events stream
//! through the `dsh-app` forwarder. The shell holds that stream instead and
//! queues its frames; the page's initialization script replaces the
//! `EventSource` for this endpoint with a polling command over the boot
//! bridge. Graph frames are full snapshots, so polling loses nothing.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;

use crate::state::ShellState;

/// Frames kept for one poll window; the graph frame is a full snapshot, so
/// dropping the oldest frames under burst only skips intermediate states.
const MAX_QUEUED_FRAMES: usize = 32;

/// Polling queue over the shell-side Server-Sent Events connection.
pub struct HmrBridge {
    frames: Mutex<VecDeque<String>>,
}

impl HmrBridge {
    /// Take every queued frame in arrival order.
    pub fn drain(&self) -> Vec<String> {
        let mut frames = self.frames.lock().expect("hmr frames lock");
        frames.drain(..).collect()
    }

    fn push(&self, frame: String) {
        let mut frames = self.frames.lock().expect("hmr frames lock");
        if frames.len() == MAX_QUEUED_FRAMES {
            frames.pop_front();
        }
        frames.push_back(frame);
    }
}

/// Start the bridge task: await Host readiness, then keep one Server-Sent
/// Events connection to the Host open, reconnecting after Host restarts.
///
/// The task re-resolves the Host origin and cookie on every attempt, so a
/// restarted Host with a fresh cookie is picked up without shell action.
pub fn start(state: Arc<ShellState>) -> Arc<HmrBridge> {
    let bridge = Arc::new(HmrBridge {
        frames: Mutex::new(VecDeque::new()),
    });
    let task_bridge = bridge.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let Some((host_origin, cookie)) = state.host_connection() else {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            };
            let url = format!("{host_origin}/plugins/events");
            let request = state
                .http_async
                .get(&url)
                .header("accept", "text/event-stream")
                .header("cookie", &cookie)
                .send()
                .await;
            match request {
                Ok(response) if response.status().is_success() => {
                    let mut stream = response.bytes_stream();
                    let mut line = Vec::new();
                    // Frames are `data: <json>` lines; comment and blank lines
                    // carry no payload and are skipped.
                    while let Some(chunk) = stream.next().await {
                        let chunk = match chunk {
                            Ok(chunk) => chunk,
                            Err(_) => break,
                        };
                        for &byte in &chunk {
                            if byte == b'\n' {
                                if line.starts_with(b"data: ") {
                                    let payload = String::from_utf8_lossy(&line[b"data: ".len()..])
                                        .trim()
                                        .to_string();
                                    if !payload.is_empty() {
                                        task_bridge.push(payload);
                                    }
                                }
                                line.clear();
                            } else {
                                line.push(byte);
                            }
                        }
                    }
                }
                Ok(response) => {
                    eprintln!(
                        "dsh desktop tauri: plugins events stream rejected with {}",
                        response.status()
                    );
                }
                Err(error) => {
                    eprintln!("dsh desktop tauri: plugins events stream failed: {error}");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    bridge
}
