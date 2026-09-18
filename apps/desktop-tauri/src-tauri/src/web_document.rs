//! Local Web document service for the application origin: static client
//! assets with the boot-readiness script, and authenticated forwarding of
//! every other request to the Desktop Host with shell-held credentials.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use tauri::http::{header, HeaderMap, HeaderValue, Method, Request, Response, StatusCode};

use crate::state::ShellState;

/// Boot gate installed into the served index; the client entry awaits it.
const BOOT_SCRIPT: &str = "<script>globalThis.__DSH_BOOT_READY__ = Promise.withResolvers()</script>";

/// Request headers the shell replaces before forwarding.
const STRIPPED_REQUEST_HEADERS: [&str; 5] = ["host", "origin", "cookie", "sec-fetch-site", "accept-encoding"];

/// Response headers owned by the forwarding hop, not the Host payload.
const STRIPPED_RESPONSE_HEADERS: [&str; 3] = ["content-encoding", "content-length", "set-cookie"];

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json") | Some("webmanifest") => "application/json",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn text_response(status: StatusCode, body: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Owned(body.as_bytes().to_vec()))
        .expect("static response builds")
}

/// Reject path components that could escape the document root.
fn root_relative(root: &Path, pathname: &str) -> Option<PathBuf> {
    let mut target = root.to_path_buf();
    for component in Path::new(pathname).components() {
        match component {
            Component::Normal(part) => target.push(part),
            Component::CurDir => {}
            Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some(target)
}

fn is_static_path(pathname: &str) -> bool {
    pathname == "/"
        || pathname == "/index.html"
        || pathname.starts_with("/assets/")
        || pathname == "/favicon.svg"
        || pathname == "/manifest.webmanifest"
}

/// Answer one `dsh-app://app` request: static client document or Host forwarding.
pub async fn handle(state: &ShellState, request: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri().clone();
    let mut pathname = uri.path().to_string();
    if pathname.is_empty() {
        pathname.push('/');
    }
    if !is_static_path(&pathname) {
        return forward(state, request, &pathname, uri.query()).await;
    }
    serve_static(state, request.method(), &pathname).await
}

async fn serve_static(state: &ShellState, method: &Method, pathname: &str) -> Response<Cow<'static, [u8]>> {
    if method != Method::GET && method != Method::HEAD {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "");
    }
    let relative = if pathname == "/" { "/index.html" } else { pathname };
    let Some(target) = root_relative(&state.paths.dist, relative) else {
        return text_response(StatusCode::FORBIDDEN, "");
    };
    // The MIME comes from the resolved file, so the index route ("/") still
    // reports text/html instead of the extensionless pathname's octet-stream.
    let mime = mime_of(&target);
    let bytes = match tokio::task::spawn_blocking(move || std::fs::read(&target)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return text_response(StatusCode::NOT_FOUND, "")
        }
        Ok(Err(error)) => {
            eprintln!("dsh desktop tauri: reading static asset failed: {error}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
        Err(join_error) => {
            eprintln!("dsh desktop tauri: static asset read task failed: {join_error}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };
    let is_index = pathname == "/" || pathname == "/index.html";
    let body: Cow<'static, [u8]> = if is_index {
        let document = String::from_utf8_lossy(&bytes);
        let served = document.replacen("<head>", &format!("<head>{BOOT_SCRIPT}"), 1);
        Cow::Owned(served.into_bytes())
    } else {
        Cow::Owned(bytes)
    };
    let body = if method == Method::HEAD { Cow::Borrowed::<[u8]>(&[]) } else { body };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .body(body)
        .expect("static response builds")
}

async fn forward(
    state: &ShellState,
    request: Request<Vec<u8>>,
    pathname: &str,
    query: Option<&str>,
) -> Response<Cow<'static, [u8]>> {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if origin.as_bytes() != crate::APP_ORIGIN.as_bytes() {
            return text_response(StatusCode::FORBIDDEN, "");
        }
    }
    let Some((host_origin, cookie)) = state.host_connection() else {
        return text_response(StatusCode::SERVICE_UNAVAILABLE, "");
    };
    let mut url = format!("{host_origin}{pathname}");
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    let method = Method::from_bytes(request.method().as_str().as_bytes()).unwrap_or(Method::GET);
    let mut headers = HeaderMap::new();
    for (name, value) in request.headers().iter() {
        if STRIPPED_REQUEST_HEADERS.iter().any(|stripped| name.as_str() == *stripped) {
            continue;
        }
        headers.insert(name.clone(), value.clone());
    }
    let mut cookie_value = HeaderValue::from_str(&cookie).expect("host cookie is header-safe ascii");
    cookie_value.set_sensitive(true);
    headers.insert(header::COOKIE, cookie_value);
    let body = request.body().clone();
    let outgoing = state.http_async.request(method, &url).headers(headers).body(body);
    let response = match outgoing.send().await {
        Ok(response) => response,
        Err(error) => {
            eprintln!("dsh desktop tauri: forwarding {url}: {error}");
            return text_response(StatusCode::BAD_GATEWAY, "");
        }
    };
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut response_headers = HeaderMap::new();
    for (name, value) in response.headers().iter() {
        if STRIPPED_RESPONSE_HEADERS.iter().any(|stripped| name.as_str() == *stripped) {
            continue;
        }
        response_headers.insert(name.clone(), value.clone());
    }
    let bytes = response.bytes().await.unwrap_or_default();
    let mut builder = Response::builder().status(status);
    for (name, value) in response_headers.iter() {
        builder = builder.header(name, value);
    }
    builder
        .body(Cow::Owned(bytes.to_vec()))
        .unwrap_or_else(|_| text_response(StatusCode::BAD_GATEWAY, ""))
}
