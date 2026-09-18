//! Loopback WebSocket bridge for the remote stream mux.
//!
//! WKWebView custom protocols cannot intercept WebSocket upgrades, and the
//! page cannot attach the Host cookie or rewrite its own Origin, so the shell
//! owns a loopback listener that relays mux frames while injecting the
//! Host-trusted Origin and cookie — the same trust position the Electron
//! shell's WebSocket header rewrite holds.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response as HandshakeResponse,
};

use crate::state::ShellState;

struct OriginGate {
    expected: String,
    path: Arc<std::sync::Mutex<Option<String>>>,
}

impl Callback for OriginGate {
    fn on_request(self, request: &Request, response: HandshakeResponse) -> Result<HandshakeResponse, ErrorResponse> {
        let origin_ok = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|origin| origin == self.expected);
        if !origin_ok {
            return Err(http::Response::builder()
                .status(403)
                .body(Some(String::from("origin rejected")))
                .expect("static rejection response"));
        }
        *self.path.lock().expect("bridge path lock") = Some(
            request
                .uri()
                .path_and_query()
                .map(|value| value.as_str().to_string())
                .unwrap_or_else(|| String::from("/api/remote.mux")),
        );
        Ok(response)
    }
}

/// Bind the bridge on an OS-assigned loopback port and start accepting.
///
/// Returns the bound port for the boot stream base origin.
pub fn start(state: Arc<ShellState>) -> std::io::Result<u16> {
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) => {
                let _ = port_tx.send(Err(error));
                return;
            }
        };
        let port = listener.local_addr().map(|address| address.port());
        if port_tx.send(port).is_err() {
            return;
        }
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let state = state.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = serve_connection(stream, &state).await {
                            eprintln!("dsh desktop tauri: stream bridge connection ended: {error}");
                        }
                    });
                }
                Err(error) => {
                    eprintln!("dsh desktop tauri: stream bridge accept failed: {error}");
                    return;
                }
            }
        }
    });
    match port_rx.recv() {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "stream bridge task exited before binding",
        )),
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    state: &Arc<ShellState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = Arc::new(std::sync::Mutex::new(None));
    let gate = OriginGate {
        expected: String::from(crate::APP_ORIGIN),
        path: path.clone(),
    };
    let page_socket = tokio_tungstenite::accept_hdr_async(stream, gate).await?;
    let Some((host_origin, cookie)) = state.host_connection() else {
        return Err("stream bridge: Host is not ready".into());
    };
    let path = path
        .lock()
        .expect("bridge path lock")
        .clone()
        .unwrap_or_else(|| String::from("/api/remote.mux"));
    // tungstenite connects over ws/wss schemes; the Host origin is plain http loopback.
    let host_url = format!("{host_origin}{path}").replacen("http://", "ws://", 1);
    let host_request = http::Request::builder()
        .uri(&host_url)
        .header("host", host_origin.trim_start_matches("http://"))
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .header("origin", &host_origin)
        .header("cookie", &cookie)
        .body(())
        .map_err(|error| format!("building host stream request: {error}"))?;
    let host_socket = tokio_tungstenite::connect_async(host_request).await?.0;
    relay(page_socket, host_socket).await
}

async fn relay<S1, S2>(page: S1, host: S2) -> Result<(), Box<dyn std::error::Error>>
where
    S1: futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
    S2: futures_util::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, tokio_tungstenite::tungstenite::Error>>
        + futures_util::Sink<tokio_tungstenite::tungstenite::Message, Error = tokio_tungstenite::tungstenite::Error>
        + Unpin,
{
    let (mut page_write, mut page_read) = page.split();
    let (mut host_write, mut host_read) = host.split();
    let page_to_host = async {
        while let Some(message) = page_read.next().await {
            match message {
                Ok(message) => host_write.send(message).await?,
                Err(error) => return Err(error),
            }
        }
        let _ = host_write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
        Ok(())
    };
    let host_to_page = async {
        while let Some(message) = host_read.next().await {
            match message {
                Ok(message) => page_write.send(message).await?,
                Err(error) => return Err(error),
            }
        }
        let _ = page_write.send(tokio_tungstenite::tungstenite::Message::Close(None)).await;
        Ok(())
    };
    let (from_page, from_host) = futures_util::join!(page_to_host, host_to_page);
    from_page?;
    from_host?;
    Ok(())
}
