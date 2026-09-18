# 桌面应用(Tauri 壳原型)

[English](README.md) | 中文

`apps/desktop-tauri` 是一个开发原型,回答一个问题:已发布的桌面薄壳架构能否改用 [Tauri 2](https://v2.tauri.app/) 而非 Electron 运行?它复用 Electron 壳复用的每一个共享件——同一个 Desktop Host、同一份 Web 客户端构建、同一条 loopback HTTP + WebSocket 传输、同样的"凭据只留在壳里"安全姿态——只更换载体。

**定位(已定)**:本壳留在仓内,与 Electron 壳并行演进。不抽取、不独立发布、不从外部引入——发布身份绑定(壳、运行时、客户端同版本)继承自 Electron 打包决策,两个载体共享仓库的准备产物与发布流程。

## 已工作的部分

- **窗口 + 自定义协议。** 原生窗口从 Rust URI-scheme 处理器加载 `dsh-app://app/`:静态客户端资源来自共享 Web dist(index 注入 `__DSH_BOOT_READY__` 门,路径带穿越防护),其余请求全部转发到 Host。
- **不依赖 Electron IPC 的 Host 监护。** Host 用系统 Node 二进制拉起,通过双工控制 socket 讲同一套生命周期协议(`apps/desktop-host/src/control.ts` 的 `DSH_DESKTOP_HOST_CONTROL=stdio`):事件出 `ready` / `fatal` / `shutdown-complete`,命令入 `shutdown`。Electron 的 `process.send` 路径不变,仍是默认。
- **凭据留在壳里。** 启动令牌 URL 在 Rust 内换取绑定 authority 的 cookie;转发请求携带它,响应剥掉 `set-cookie`。页面永远见不到 cookie。
- **流桥。** WKWebView 自定义协议拦不住 WebSocket 升级,页面也无法附加 Host cookie 或改写自己的 `Origin`,因此壳持有一个 loopback WebSocket 中继,以 Host 信任的 `Origin` 和 cookie 升级到 Host mux。页面只通过 boot 握手得知中继 origin(`streamBaseUrl`)。
- **插件事件桥。** Host 的 `/plugins/events` Server-Sent Events 通道(插件启用/禁用与重建通知,含初始完整插件图)无法穿过自定义协议——协议只能以完整响应体作答。壳持着这条流(带 Host cookie)并把帧排队(`hmr_bridge.rs`);初始化脚本把该端点的页面 `EventSource` 替换为每秒一次的轮询命令。graph 帧是全量快照,轮询不损失 reconcile 语义——Agent Team 顶部按钮等浏览器插件现在随插件开关出现、消失、再出现,与 Electron 一致。
- **boot 桥。** 初始化脚本暴露 `dshDesktopBoot.ready/failed`(Tauri 命令)并标记 `data-platform`,未修改的 Web 客户端入口与 Electron 下完全一致地启动。`dshDesktop` 只暴露协议版本桩,更新 UI 保持隐藏。
- **通过维护中的 Tauri 生态完成原生集成。** `tauri-plugin-dialog` 负责致命消息框与窗口归属的目录选择器(`__DSH_DIRECTORY_PICKER__`),`tauri-plugin-single-instance` 负责 profile 锁,`window-vibrancy` 加 `TitleBarStyle::Overlay`(及其要求的 `macos-private-api` 透明)负责 macOS 侧栏材质,`AppHandle::set_theme` 镜像页面的 `data-ds-theme-source` 调色板使 vibrancy 跟随应用主题。macOS 应用菜单——应用子菜单下的 About 面板、Services、隐藏命令与 Quit,加上标准 Edit 与 Window 子菜单——用 `tauri::menu` 组装。
- **生命周期。** 单实例锁;导航限制在 `dsh-app:` 与 loopback HTTP;Host 分级停机(请求 → SIGTERM → SIGKILL)。

## 开发

```sh
pnpm run dev:desktop-tauri    # build, prepare the shared dev project, launch tauri dev
pnpm run start:desktop-tauri  # relaunch with existing artifacts
```

启动器复用 `apps/desktop` 的准备逻辑:一次性开发工程、primary runtime 载荷,以及新增的——隔离的 `home-tauri` Harness home 的 profile 初始化(`apps/desktop/.desktop-build/development/home-tauri`),Tauri 壳绝不碰 Electron 的开发 home 或用户的 `~/.dsh`。要求:Rust 工具链、PATH 上有 Node `^22.19 || >=24`。

## 已知差距(原型有意裁剪)

| 差距 | Electron 对应 |
|---|---|
| 无自动更新、强制更新策略、更新任务控制;打包就绪后生态路径是 `tauri-plugin-updater` | `apps/desktop/src/update-*.ts`、`mandatory-update-*.ts` |
| 无打包布局:不内置 Node/pnpm、无 `desktop-runtime.json` 校验、无签名 | `apps/desktop/scripts/prepare-*.ts`、`runtime-tree.ts` |
| profile 初始化在开发启动器而非壳启动时执行 | `main.ts` 的 `backend.start` 回调 |
| 无 Windows 标题栏 overlay 与 IME 菜单 | `main.ts` 菜单/窗口部分 |
| 红绿灯位置用 macOS 默认内边距,不是 Electron 的 `(16, 18)` | `main.ts` 窗口配置 |
| 流中继接纳任何携带 `dsh-app://app` origin 的 loopback 客户端;Electron 把改写绑定到主窗口 `webContentsId` | `main.ts` 的 WebSocket 改写 |
| 致命错误渲染进页面并弹原生消息框,但没有禁用插件的恢复流程 | `fatal-recovery.ts` |
| Windows/Linux 未接线(控制 socket 与开发路径为 unix 专用) | — |

载体决策记录见 `.agents/notes/implemented/architecture/2026-09-10-desktop-web-wrapper.zh.md`;本原型遵循该记录,只在 Tauri 的 webview 模型迫使采用等价机制处偏离(流中继替代头改写、socket 控制通道替代 Node IPC)。
