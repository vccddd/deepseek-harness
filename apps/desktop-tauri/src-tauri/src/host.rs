//! Desktop Host supervision: spawn the Node host over a duplex control
//! socket, authenticate its launch URL into a shell-held cookie, and stop it
//! with escalating termination on exit.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::state::{ShellState, Startup};

/// Ceiling for retained Host stderr, mirroring the Electron shell's bound.
const MAX_STDERR_CHARS: usize = 64 * 1024;

/// Running Desktop Host child with its control socket.
struct HostChild {
    child: Child,
    control: UnixStream,
    stopping: Arc<AtomicBool>,
}

/// Supervised handle exposed to the shell exit path.
pub struct HostHandle {
    inner: Mutex<Option<HostChild>>,
}

impl HostHandle {
    /// Stop the Host: request shutdown, then escalate to SIGTERM and SIGKILL.
    pub fn stop_blocking(&self) {
        let mut guard = self.inner.lock().expect("host lock");
        let Some(HostChild { child, control, stopping, .. }) = guard.as_mut() else {
            return;
        };
        if stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = writeln!(control, "{{\"type\":\"shutdown\"}}");
        let _ = control.flush();
        if wait_exit(child, Duration::from_secs(10)) {
            return;
        }
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        if wait_exit(child, Duration::from_secs(5)) {
            return;
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn wait_exit(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

/// Launch the Host supervisor threads; readiness flows into `state`.
pub fn start(state: Arc<ShellState>) -> Arc<HostHandle> {
    let handle = Arc::new(HostHandle { inner: Mutex::new(None) });
    let thread_handle = handle.clone();
    std::thread::spawn(move || {
        if let Err(error) = supervise(thread_handle.clone(), &state) {
            eprintln!("dsh desktop tauri: host supervisor failed: {error}");
            if !matches!(state.startup(), Startup::Ready { .. }) {
                state.publish(Startup::Failed(error.clone()));
                state.show_fatal(&error);
            }
            // Release the child slot so exit-time stop is a no-op after a spawn failure.
            *thread_handle.inner.lock().expect("host lock") = None;
        }
    });
    handle
}

fn supervise(handle: Arc<HostHandle>, state: &Arc<ShellState>) -> Result<(), String> {
    let paths = &state.paths;
    let entry = paths
        .dsh_dir
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh-desktop-host")
        .join("lib")
        .join("index.js");
    if !entry.is_file() {
        return Err(format!("dsh desktop tauri: missing Desktop Host entry {}", entry.display()));
    }
    let (shell_end, host_end) = UnixStream::pair().map_err(|e| format!("control socketpair: {e}"))?;
    // SAFETY: into_raw_fd releases ownership of the descriptor; from_raw_fd
    // retakes it, so the socket closes exactly once.
    let host_stdin = unsafe { std::os::fd::OwnedFd::from_raw_fd(host_end.into_raw_fd()) };
    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let mut child = Command::new(&paths.node)
        .arg("--expose-internals")
        .arg(&entry)
        .arg(&paths.dsh_dir)
        .arg(&paths.profile)
        .arg(&paths.primary_runtime)
        .arg("link")
        .arg(&paths.pnpm)
        .arg(&paths.node_bin)
        .env("DSH_HOME", &paths.home)
        .env("DSH_DESKTOP_HOST_CONTROL", "stdio")
        .env("DSH_DESKTOP_HOST_CONTROL_FD", "0")
        .env("DSH_DESKTOP_NODE_EXECUTABLE", &paths.node)
        .env("PATH", format!("{}:{inherited_path}", paths.node_bin.display()))
        .stdin(Stdio::from(host_stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawning Desktop Host with {}: {e}", paths.node))?;

    let stdout = child.stdout.take().expect("host stdout piped");
    let stderr = child.stderr.take().expect("host stderr piped");
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            println!("[dsh-desktop-host] {line}");
        }
    });
    {
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("[dsh-desktop-host] {line}");
                let mut tail = tail.lock().expect("stderr tail lock");
                let combined = tail.len() + line.len() + 1;
                if combined > MAX_STDERR_CHARS {
                    let start = tail
                        .char_indices()
                        .nth(line.len() + 1)
                        .map(|(at, _)| at)
                        .unwrap_or(tail.len());
                    tail.drain(..start);
                }
                tail.push_str(&line);
                tail.push('\n');
            }
        });
    }

    let stopping = Arc::new(AtomicBool::new(false));
    let reader = shell_end
        .try_clone()
        .map_err(|e| format!("cloning control socket: {e}"))?;
    *handle.inner.lock().expect("host lock") = Some(HostChild {
        child,
        control: shell_end,
        stopping: stopping.clone(),
    });

    // Event reader owns the control socket read side until the Host exits.
    read_host_events(reader, state)?;
    if stopping.load(Ordering::SeqCst) || matches!(state.startup(), Startup::Failed(_)) {
        return Ok(());
    }
    // The control socket reached EOF: the Host process is exiting.
    let exit = {
        let mut guard = handle.inner.lock().expect("host lock");
        match guard.as_mut() {
            Some(HostChild { child, .. }) => {
                child.wait().map_err(|e| format!("waiting for Desktop Host exit: {e}"))?
            }
            None => return Ok(()),
        }
    };
    let tail = stderr_tail.lock().expect("stderr tail lock").clone();
    let suffix = if tail.trim().is_empty() { String::new() } else { format!(": {}", tail.trim()) };
    Err(format!("Desktop Host exited with {exit}{suffix}"))
}

fn read_host_events(reader: UnixStream, state: &Arc<ShellState>) -> Result<(), String> {
    let reader = BufReader::new(reader);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("control socket: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = match serde_json::from_str(&line) {
            Ok(event) => event,
            Err(_) => return Err(format!("Desktop Host sent an invalid control frame: {line}")),
        };
        match event.get("type").and_then(Value::as_str) {
            Some("ready") => {
                let url = event
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| String::from("Desktop Host ready frame has no url"))?;
                let injections = event.get("injections").cloned().unwrap_or(Value::Array(Vec::new()));
                let cookie = authenticate(url, &state.http)?;
                let origin = origin_of(url)?;
                state.publish(Startup::Ready { host_origin: origin, cookie, injections });
            }
            Some("fatal") => {
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Desktop Host reported a fatal error");
                state.publish(Startup::Failed(format!("Desktop Host: {message}")));
                state.show_fatal(message);
                return Ok(());
            }
            // shutdown-complete and update-tasks replies need no supervisor action here.
            _ => {}
        }
    }
    Ok(())
}

/// Exchange the Host launch URL for its authority-bound cookie, mirroring the
/// Electron shell: expect the 303 redirect and keep only the cookie pair.
fn authenticate(url: &str, http: &reqwest::blocking::Client) -> Result<String, String> {
    let response = http
        .get(url)
        .send()
        .map_err(|e| format!("Desktop Host authentication request failed: {e}"))?;
    let status = response.status();
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or(value).to_string());
    match (status.as_u16(), cookie) {
        (303, Some(cookie)) => Ok(cookie),
        (status, _) => Err(format!("Desktop Host authentication failed ({status})")),
    }
}

fn origin_of(url: &str) -> Result<String, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Desktop Host url: {e}"))?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| String::from("Desktop Host url has no port"))?;
    Ok(format!(
        "{}://{}:{port}",
        parsed.scheme(),
        parsed.host_str().unwrap_or("127.0.0.1")
    ))
}
