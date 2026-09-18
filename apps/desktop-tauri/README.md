# Desktop application (Tauri shell prototype)

English | [中文](README.zh.md)

`apps/desktop-tauri` is a development prototype that answers one question: can the shipped desktop thin-wrapper architecture run on [Tauri 2](https://v2.tauri.app/) instead of Electron? It reuses every shared piece the Electron shell reuses — the same Desktop Host, the same Web client build, the same loopback HTTP + WebSocket transport, and the same credential-in-the-shell security posture — and changes only the carrier.

**Positioning (decided):** this shell stays in-repo and evolves in parallel with the Electron shell. It is not extracted, published, or imported from elsewhere — the release-identity binding (one version for shell, runtime, and client) is inherited from the Electron packaging decision, and both carriers share the repository's preparation artifacts and release flow.

## What works

- **Window + custom protocol.** A native window loads `dsh-app://app/` from a Rust URI-scheme handler: static client assets from the shared Web dist (with the `__DSH_BOOT_READY__` gate injected into the index), path-traversal-fenced, everything else forwarded to the Host.
- **Host supervision without Electron IPC.** The Host is spawned with the system Node binary and speaks the same lifecycle protocol over a duplex control socket (`DSH_DESKTOP_HOST_CONTROL=stdio` in `apps/desktop-host/src/control.ts`): `ready` / `fatal` / `shutdown-complete` events in, `shutdown` out. The Electron `process.send` path is unchanged and remains the default.
- **Credentials stay in the shell.** The launch-token URL is exchanged for the authority-bound cookie inside Rust; forwarded requests carry it, responses drop `set-cookie`. The page never sees the cookie.
- **Stream bridge.** WKWebView custom protocols cannot intercept WebSocket upgrades, and a page cannot attach the Host cookie or rewrite its own `Origin`, so the shell owns a loopback WebSocket relay that upgrades to the Host mux with the Host-trusted `Origin` and cookie. The page learns only the relay origin through the boot handshake (`streamBaseUrl`).
- **Boot bridge.** An initialization script exposes `dshDesktopBoot.ready/failed` (Tauri commands) and marks `data-platform`, so the unmodified Web client entry boots exactly as under Electron. `dshDesktop` is exposed as the protocol-version-only stub; update UI stays hidden.
- **Native integration through the maintained Tauri ecosystem.** `tauri-plugin-dialog` owns fatal message boxes and the window-owned directory picker (`__DSH_DIRECTORY_PICKER__`), `tauri-plugin-single-instance` owns the profile lock, `window-vibrancy` plus `TitleBarStyle::Overlay` (and the `macos-private-api` transparency it requires) owns the macOS sidebar material, and `AppHandle::set_theme` mirrors the page's `data-ds-theme-source` palette so vibrancy follows the app theme. The macOS standard Edit menu and its shortcuts come from Tauri's default menu.
- **Lifecycle.** Single-instance lock, navigation fenced to `dsh-app:` and loopback HTTP, escalating Host shutdown (request → SIGTERM → SIGKILL).

## Development

```sh
pnpm run dev:desktop-tauri    # build, prepare the shared dev project, launch tauri dev
pnpm run start:desktop-tauri  # relaunch with existing artifacts
```

The launcher reuses `apps/desktop` preparation: the disposable development project, the primary runtime payload, and — new — profile initialization for the isolated `home-tauri` Harness home (`apps/desktop/.desktop-build/development/home-tauri`), so the Tauri shell never touches the Electron dev home or the user's `~/.dsh`. Requirements: Rust toolchain, Node `^22.19 || >=24` on PATH.

## Known gaps (deliberate prototype scope)

| Gap | Electron reference |
|---|---|
| No auto-update, mandatory-update policy, or update task control; `tauri-plugin-updater` is the ecosystem path when packaging arrives | `apps/desktop/src/update-*.ts`, `mandatory-update-*.ts` |
| No packaged layout: no bundled Node/pnpm, no `desktop-runtime.json` verification, no signing | `apps/desktop/scripts/prepare-*.ts`, `runtime-tree.ts` |
| Profile initialization lives in the dev launcher, not the shell startup | `main.ts` `backend.start` callback |
| No custom application menu or About panel (macOS default Edit menu is present); no Windows titlebar overlay or IME menu plumbing | `main.ts` menu/window sections |
| Traffic-light positions use the macOS default inset, not Electron's `(16, 18)` | `main.ts` window config |
| Stream relay admits any loopback client with a `dsh-app://app` origin; Electron binds the rewrite to the main window's `webContentsId` | `main.ts` WebSocket rewrite |
| Fatal errors render into the page plus a native message box, without the plugin-disable recovery flow | `fatal-recovery.ts` |
| Windows/Linux are not wired (the control socket and dev paths are unix-only) | — |

The full carrier decision record lives in `.agents/notes/implemented/architecture/2026-09-10-desktop-web-wrapper.zh.md`; this prototype follows it and diverges only where Tauri's webview model forces an equivalent mechanism (stream relay instead of header rewrite, socket control channel instead of Node IPC).
