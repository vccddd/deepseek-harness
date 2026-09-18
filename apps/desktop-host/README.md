# Desktop Host

English | [中文](README.zh.md)

`@deepseek-ai/dsh-desktop-host` is the private Node-mode host process a desktop shell spawns to run the shared Web application backend. It owns no product logic of its own: it boots the desktop profile, reports the authenticated application URL to the shell, and then serves the page through loopback HTTP and WebSocket. Two carriers spawn it unchanged — the Electron shell (`apps/desktop`) and the Tauri shell prototype (`apps/desktop-tauri`) — and both speak the control channel specified here.

## Control channel

The Host speaks one lifecycle protocol with its launching shell. The transport is selected at process start by `createDesktopHostControl` (`src/control.ts`):

| Transport | Selection | Framing |
|---|---|---|
| Electron IPC | Default: no `DSH_DESKTOP_HOST_CONTROL` in the environment | Node IPC channel (V8 serialization over fd 3) |
| Duplex socket | `DSH_DESKTOP_HOST_CONTROL=stdio` | Newline-delimited JSON: commands arrive on stdin (fd 0), events are written to the write side of the descriptor named by `DSH_DESKTOP_HOST_CONTROL_FD` (default `0`) |

The stdio transport exists for non-Electron shells. The Tauri shell passes one end of a Unix socketpair as the Host's stdin and reads events from the same socket; the Electron path is byte-for-byte unchanged when the environment variable is unset. Both transports deliver the same message set — `tests/control.spec.ts` runs the same command stream through a real IPC channel (forked `tests/control-ipc-fixture.ts`) and a stdio pair and asserts equal delivery.

## Commands (shell → Host)

| Frame | Fields | Meaning |
|---|---|---|
| `{"type":"shutdown"}` | — | Request teardown. The Host stops the profile, sends `shutdown-complete`, and exits. |
| `{"type":"update-tasks","requestId":N,"action":"inspect"\|"lock"\|"unlock"}` | `requestId` is a shell-assigned safe integer echoed in the reply; `action` reads active work, takes the update admission lock, or releases it | Update-task control used by the shell's update flow. |

Shells escalate when `shutdown` goes unanswered: the Electron shell waits 10 s, sends SIGTERM, waits 5 s, then SIGKILL (`apps/desktop/src/host-process.ts` `stop()`); the Tauri shell applies the same ladder (`apps/desktop-tauri/src-tauri/src/host.rs` `stop_blocking()`). An unanswered `update-tasks` command fails at the shell's control-request deadline (10 s in the Electron shell); a missing reply never authorizes an install.

## Events (Host → shell)

| Frame | Fields | Meaning |
|---|---|---|
| `{"type":"ready","url":"...","injections":[...]}` | `url` is the launch-token authentication URL the shell exchanges for its held cookie; `injections` is the index injection table (`IndexInjection` values) forwarded verbatim into the boot handshake | Sent exactly once after the profile booted. |
| `{"type":"fatal","message":"..."}` | — | Startup failed. The Host then exits with code 1. |
| `{"type":"shutdown-complete"}` | — | Reply to `shutdown`: the profile tree is down. Unsolicited `shutdown-complete` is a protocol violation. |
| `{"type":"update-tasks","requestId":N,"active":bool,"error":"..."?}` | Reply pairs with the command by `requestId`. `error` present means the request was refused (for example the Host is stopping); `active` states whether live tasks would be affected | — |

## Error semantics

On the Host side, an unparseable JSON line is dropped silently, and a parseable frame that is not a valid command is ignored silently (`parseDesktopHostCommand`). The channel assumes a trusted shell: it validates frame shape, not hostile input. On the shell side, an event frame that fails shape validation is fatal — the Electron shell fails the host and sends SIGTERM; the Tauri supervisor treats an invalid control frame as a fatal error. Channel EOF or IPC disconnect means the shell is gone: the Host tears itself down without waiting for `shutdown`.

## Process contract

The Host is spawned as `node --expose-internals <entry> runtimeDir projectDir primaryRuntime resolution pnpm nodeBin` (`src/index.ts`; both shells pass this exact argv). `ready` is the first event on a healthy channel and is sent exactly once. The environment sets `DSH_HOME` for the profile home and (stdio transport only) `DSH_DESKTOP_HOST_CONTROL` plus `DSH_DESKTOP_HOST_CONTROL_FD`.

## Version governance

The protocol carries no version field inside its frames. Compatibility is governed by `DESKTOP_HOST_PROTOCOL_VERSION` (`apps/desktop/src/host-protocol.ts`, currently `4`), recorded as `hostProtocolVersion` in `desktop-runtime.json` and verified against the shell's expectation at release-manifest load (`apps/desktop/src/release.ts`). Any change to the message set, a field's meaning, or the error semantics above must bump this constant in the same PR and update both shell consumers, the Host, and the parity tests together.

## Package layout

- `src/control.ts` — transport selection and both transports (public surface for shells and tests).
- `src/index.ts` — Host entry: profile boot, ready reporting, command handling.
- `src/update-tasks.ts`, `src/office.ts`, `src/primary-runtime.ts` — Host-side behaviors behind the channel.
- `tests/control.spec.ts`, `tests/control-ipc-fixture.ts` — protocol and transport-parity tests.
