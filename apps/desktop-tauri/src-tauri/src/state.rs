//! Shared shell state: host startup facts, the stream bridge origin, and the
//! application window handle used for fatal presentation.

use std::sync::{Arc, RwLock};

use tokio::sync::watch;

use crate::hmr_bridge::HmrBridge;
use crate::host::HostHandle;
use crate::paths::ShellPaths;

/// Boot facts the shell holds in memory; the auth cookie never reaches the page.
#[derive(Debug, Clone)]
pub enum Startup {
    Pending,
    Ready {
        /** Loopback origin the Host listens on. */
        host_origin: String,
        /** Authority-bound Host cookie, held only by the shell. */
        cookie: String,
        /** Boot injection table reported by the Host. */
        injections: serde_json::Value,
    },
    Failed(String),
}

/// Process-wide state shared by the protocol handler, the WebSocket bridge,
/// the boot commands, and the Host supervisor threads.
pub struct ShellState {
    /// Development filesystem roots.
    pub paths: ShellPaths,
    /// Blocking loopback HTTP client used for the Host launch-token exchange.
    pub http: reqwest::blocking::Client,
    /// Async loopback HTTP client used by the protocol forwarder.
    pub http_async: reqwest::Client,
    startup_tx: watch::Sender<Startup>,
    startup_rx: watch::Receiver<Startup>,
    stream_base: RwLock<Option<String>>,
    window: RwLock<Option<tauri::WebviewWindow>>,
    app: RwLock<Option<tauri::AppHandle>>,
    host: RwLock<Option<Arc<HostHandle>>>,
    hmr: RwLock<Option<Arc<HmrBridge>>>,
}

impl ShellState {
    /// Create pending state for the resolved development roots.
    pub fn new(paths: ShellPaths) -> Self {
        let (startup_tx, startup_rx) = watch::channel(Startup::Pending);
        Self {
            paths,
            http: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest blocking client builds without TLS features"),
            http_async: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest async client builds without TLS features"),
            startup_tx,
            startup_rx,
            stream_base: RwLock::new(None),
            window: RwLock::new(None),
            app: RwLock::new(None),
            host: RwLock::new(None),
            hmr: RwLock::new(None),
        }
    }

    /// Current startup snapshot.
    pub fn startup(&self) -> Startup {
        self.startup_rx.borrow().clone()
    }

    /// Host origin and cookie once the Host authenticated; `None` while pending or failed.
    pub fn host_connection(&self) -> Option<(String, String)> {
        match self.startup() {
            Startup::Ready { host_origin, cookie, .. } => Some((host_origin, cookie)),
            _ => None,
        }
    }

    /// Publish readiness (or the first failure) and wake boot waiters.
    pub fn publish(&self, startup: Startup) {
        let _ = self.startup_tx.send(startup);
    }

    /// Await the boot answer for the page: injections plus the stream bridge origin.
    pub async fn wait_for_boot(&self) -> Result<serde_json::Value, String> {
        let mut rx = self.startup_rx.clone();
        loop {
            match &*rx.borrow() {
                Startup::Pending => {}
                Startup::Ready { injections, .. } => {
                    return Ok(serde_json::json!({
                        "injections": injections,
                        "streamBaseUrl": self.stream_base(),
                    }))
                }
                Startup::Failed(message) => return Err(message.clone()),
            }
            if rx.changed().await.is_err() {
                return Err(String::from("dsh desktop tauri: shell state dropped"));
            }
        }
    }

    /// Register the WebSocket bridge origin returned to the page.
    pub fn set_stream_base(&self, origin: String) {
        *self.stream_base.write().expect("stream base lock") = Some(origin);
    }

    /// Registered stream bridge origin, once the listener bound.
    pub fn stream_base(&self) -> String {
        self.stream_base
            .read()
            .expect("stream base lock")
            .clone()
            .unwrap_or_else(|| String::from("http://127.0.0.1:0"))
    }

    /// Remember the application window for fatal presentation from Host threads.
    pub fn set_window(&self, window: tauri::WebviewWindow) {
        *self.window.write().expect("window lock") = Some(window);
    }

    /// Remember the app handle for native fatal dialogs from Host threads.
    pub fn set_app(&self, app: tauri::AppHandle) {
        *self.app.write().expect("app lock") = Some(app);
    }

    /// Present a fatal message on the loaded document and in a native dialog.
    pub fn show_fatal(&self, message: &str) {
        if let Some(window) = self.window.read().expect("window lock").clone() {
            let escaped = serde_json::to_string(message).unwrap_or_else(|_| String::from("\"\""));
            let script = format!(
                "document.open();document.write('<pre style=\"white-space:pre-wrap;font:13px ui-monospace,monospace;padding:24px\">dsh desktop: fatal\\n'+{escaped}+'</pre>');document.close();"
            );
            if let Err(error) = window.eval(&script) {
                eprintln!("dsh desktop tauri: fatal page eval failed: {error}");
            }
        }
        if let Some(app) = self.app.read().expect("app lock").clone() {
            let message = message.to_string();
            std::thread::spawn(move || {
                use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                app.dialog()
                    .message(format!("dsh desktop: fatal\n{message}"))
                    .title("DeepSeek Harness")
                    .kind(MessageDialogKind::Error)
                    .blocking_show();
            });
        }
    }

    /// Register the Host supervisor handle owned by the exit path.
    pub fn set_host(&self, host: Arc<HostHandle>) {
        *self.host.write().expect("host lock") = Some(host);
    }

    /** Owned Host supervisor, when the Host was launched. */
    pub fn host(&self) -> Option<Arc<HostHandle>> {
        self.host.read().expect("host lock").clone()
    }

    /// Register the plugins-events bridge owned by the boot bridge commands.
    pub fn set_hmr(&self, hmr: Arc<HmrBridge>) {
        *self.hmr.write().expect("hmr lock") = Some(hmr);
    }

    /// Owned plugins-events bridge, when the shell started it.
    pub fn hmr(&self) -> Option<Arc<HmrBridge>> {
        self.hmr.read().expect("hmr lock").clone()
    }
}
