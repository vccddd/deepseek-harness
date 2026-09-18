# DeepSeek Harness 桌面端 Tauri 壳开发指南:对标 Electron 的实现解读

> 基于 `desktop-tauri` 分支的 `apps/desktop-tauri` 原型整理。与 [desktop-development-guide.md](desktop-development-guide.md)(Electron 版解读)配套阅读:那一份讲"薄壳架构为什么这么设计",这一份讲"同一套设计换到 Tauri 载体上,每个部分怎么实现、为什么这样实现"。面向**非 Tauri 背景**的开发者,只需具备一般 Web 与 Node.js 知识。所有设计理由分两类:继承自 Electron 版的(直接引用其决策记录),Tauri 模型迫使做出的等价替换(标注代码路径)。

---

## 0. 五分钟 Tauri 背景

读后面的内容只需要这五个概念:

| 概念 | 是什么 | 与 Electron 的对应 |
|---|---|---|
| **核心进程 (Rust)** | 壳的本体,用 Rust 写,拥有窗口、协议处理、子进程、网络等全部系统能力 | 主进程 (main),但语言是 Rust 且**没有内置 Node.js** |
| **系统 WebView** | 不捆绑 Chromium:macOS 用 WKWebView,Windows 用 WebView2。页面跑在操作系统自带的浏览器引擎里 | 渲染进程 (renderer),但引擎由系统提供,行为细节随系统版本 |
| **自定义协议** | 向 WebView 注册一个 URL scheme(本文 `dsh-app://`),Rust 侧的回调接管该 scheme 的全部请求 | `protocol.handle` + `registerSchemesAsPrivileged` |
| **命令 (command)** | 页面通过 `invoke('命令名', 参数)` 调用 Rust 函数;Rust 用 `#[tauri::command]` 导出 | `ipcMain.handle` + preload 里的 `ipcRenderer.invoke` |
| **初始化脚本** | 建窗口时注入的一段 JS,**先于页面所有脚本**运行,是页面够到 Rust 的唯一窄通道 | preload 脚本 |

再加两个影响设计的差异:

- **插件生态**:`tauri-plugin-*` 官方插件承担 Electron 的内建能力(对话框、单实例、自动更新各有插件)。本壳只用了对话与本窗体相关的几个(见 §2.7)。
- **私有 API 开关**:macOS 上要让 WebView 背景透明(毛玻璃的 prerequisite),必须开启 `macos-private-api` Cargo feature 并在 `tauri.conf.json` 声明 `macOSPrivateApi: true`,两者必须配对,否则构建脚本报错。Electron 内部走的等价路径,对开发者不可见。

威胁模型与 Electron 版完全一致:**页面(整个 Web UI)不可信,假设被 XSS 攻陷,攻击者也拿不到宿主凭据、文件系统、shell 与任意包管理操作**。后文安全部分(§3)逐条对照防线。

---

## 1. 总体架构:同一副薄壳,不同的载体

```
┌─────────────────────────────────────────────────────────────┐
│ Tauri 核心进程 (apps/desktop-tauri, Rust)                    │
│  窗口/材质 · dsh-app:// 协议 · WS 流桥 · Host 监护 · 更新留空 │
└──────────┬──────────────────────────────┬───────────────────┘
           │ 双工控制 socket (JSON 行)      │ HTTP 转发 + WS 中继
           ▼                              │
┌─────────────────────────┐               │
│ Desktop Host 子进程      │◄──────────────┘
│ (系统 Node 运行,        │   127.0.0.1:19387 (loopback)
│  共享 Web profile        │
│  + agent 运行时)         │
└─────────────────────────┘
           ▲ 加载 (dsh-app://app/)
┌──────────┴────────────────┐
│ 系统 WebView = 共享 Web SPA│
│ (apps/web 构建产物,        │
│  与浏览器版完全同一份)      │
└───────────────────────────┘
```

**四个应用的关系与 Electron 版一字不差**:`apps/desktop-tauri` 是壳(不含产品逻辑),`apps/desktop-host` 是私有 Node 宿主,`apps/web` 的 dist 是共享 UI,`apps/cli` 提供 profile 启动逻辑。薄壳决策(第二套后端必然遗漏 Web 功能)原样继承 `2026-09-10-desktop-web-wrapper.zh.md`,此处不复述。

与 Electron 版的两个结构性差异:

### 1.1 Host 的 Node 从哪来

Electron 用 `ELECTRON_RUN_AS_NODE` 拿自己当 Node 运行时(`2026-09-11` 决策)。Tauri 是纯 Rust 二进制,**没有任何 JS 运行时**,所以:

| 场景 | Node 来源 | 代码 |
|---|---|---|
| 开发模式 | 系统 `node`(PATH),启动时用 `node -p process.execPath` 解析成**绝对路径** | `paths.rs` `resolve_node()` |
| 打包模式(未实现,路径已留) | primary-runtime 已捆绑的固定版本 Node | `paths.rs` 的 target 推导 |

为什么必须绝对路径:`apps/desktop/scripts/node-bin/node` 是个私有启动器,内容是 `exec "$DSH_DESKTOP_NODE_EXECUTABLE" --expose-internals "$@"`,且它的目录被前置进 Host 的 PATH。如果 `DSH_DESKTOP_NODE_EXECUTABLE` 是相对名 `node`,PATH 解析再次命中启动器自身,**无限递归并每层追加一个 `--expose-internals`**——这是实测踩过的坑(§7)。

### 1.2 壳 ↔ Host 控制通道:双工 socket + JSON 行

Electron 用 Node IPC(stdio `'ipc'`,fd 3,V8 序列化帧)传生命周期消息。Rust 没有现成的 Node IPC 帧实现,也不该手写一个脆弱的。方案:

- 壳创建一个 **Unix socketpair**,一端作为 Host 子进程的 **stdin**;
- socket 是全双工的:命令(壳→Host)写在页面上就是 Host 的 stdin 流,事件(Host→壳)由 Host 对 fd 0 的写方向返回(Node 侧 `fs.createWriteStream('', { fd: 0 })`);
- 帧格式是**换行分隔的 JSON**,消息集合与 Electron 版逐一相同:`ready`(url + 注入表)、`fatal`、`shutdown-complete`、`update-tasks`。

产品侧的唯一改动在 `apps/desktop-host/src/control.ts`:新增 env 门控的 stdio 传输(`DSH_DESKTOP_HOST_CONTROL=stdio`),**不设此环境变量时走原有 Node IPC 路径,Electron 行为零变化**。这让它成为一个可测试的纯增量(有独立单测 `apps/desktop-host/tests/control.spec.ts`)。

---

## 2. 各部分实现对照

以下每节先给"Electron 怎么做 → Tauri 怎么做"的映射,再讲达到的目的。

### 2.1 窗口与平台材质(macOS)

| Electron (`apps/desktop/src/main.ts`) | Tauri (`src-tauri/src/main.rs` setup) |
|---|---|
| `titleBarStyle: 'hiddenInset'` + `trafficLightPosition` | `TitleBarStyle::Overlay`(红绿灯悬浮于内容,等价形态) |
| `vibrancy: 'sidebar'` + `visualEffectState: 'active'` + 透明背景 | `window-vibrancy` crate 的 `apply_vibrancy(Sidebar, Active)` + `.transparent(true)`(依赖 `macos-private-api`) |
| 红绿灯坐标 `(16, 18)` | 无公开 API,用系统默认 inset(已记录为差距) |

目的不变:侧栏毛玻璃跟随应用而非系统,UI 的 `data-platform="darwin"` CSS 分支为红绿灯留白。**主题镜像**也照搬了:`preload-theme.ts` 监视 `html[data-ds-theme-source]` 转发给 `nativeTheme.themeSource`;Tauri 版由初始化脚本里的同款 MutationObserver 调 `desktop_set_theme` 命令,落到 `AppHandle::set_theme(Light/Dark/None)`。

### 2.2 `dsh-app://` 协议:本地文档 + 认证转发

这是壳的心脏,两侧几乎逐行同构(`web-document.ts` ↔ `web_document.rs`):

| 行为 | 共同实现 |
|---|---|
| 静态路径白名单 | `/`、`/index.html`、`/assets/*`、favicon、manifest 从 dist 读;**其余全部转发 Host** |
| boot 门 | index 的 `<head>` 后注入 `__DSH_BOOT_READY__ = Promise.withResolvers()` |
| 路径穿越防护 | 解析后必须在 dist 目录内(拒绝 `..` 组件),否则 403 |
| 转发请求头 | 删 `host/origin/cookie/sec-fetch-site`,代入壳持有的 Host cookie;origin 存在且非 `dsh-app://app` 时 403 |
| 转发响应头 | 删 `content-encoding/content-length/set-cookie`(宿主无法借道种 cookie) |
| 未就绪 | 503,页面加载页继续等待 |

差异只有执行模型:Electron 在主进程用 Node fetch;Tauri 用 `register_asynchronous_uri_scheme_protocol` 注册异步处理器,每个请求 `tauri::async_runtime::spawn` 一个异步任务(静态文件读走 `spawn_blocking`,HTTP 走异步 reqwest)。**目的**:自定义协议天生同源——页面请求 `dsh-app://app/api/...` 即同源请求,不需要教 Web 客户端任何新传输,这正是"复用 Web 的 HTTP+WS"决策(§2.2 of Electron 版)在 Tauri 上的落点。

实现细节里有一个必须写进注释的坑:**MIME 必须取自解析后的目标文件而不是 URL 路径**。根路径 `/` 没有扩展名,误用路径查表会返回 `application/octet-stream`,WKWebView 把导航当下载拒绝渲染,页面从此不发任何资源请求——表象是"协议 200 但白屏"(§7 有完整复盘)。

### 2.3 凭据:cookie 只存在于 Rust 内存

与 Electron `authenticateWebHost` 同构:Host ready 后回报带 launch token 的 URL,壳以**禁止重定向**的请求访问它,期望 303 + `set-cookie`,截取 cookie 名值对存进 `ShellState`(`host.rs` `authenticate()`)。此后:

- 每个转发的 `/api` 请求由 Rust 附加该 cookie(响应头标记 sensitive,不进日志);
- WS 中继由 Rust 附加(§2.4);
- 页面从头到尾见不到 cookie,其源是 `dsh-app://`,浏览器跨源保护照常生效。

净效果与 Electron 版 §3.4 相同:XSS 拿不到可外传的持久凭据。

### 2.4 WebSocket 流桥:Electron 没有的新组件

**为什么必须有**:页面的流式通道是 `ws://127.0.0.1:19387/api/remote.mux`,而 Host 对升级请求做两道校验——Origin 必须等于 Host authority,且必须携带绑定 authority 的签名 cookie(见 `packages/client/connection/src/api-request-trust.ts` 与 `browser-auth.ts`)。Electron 主进程能按 `webContentsId` 精确改写主窗口 WS 请求的 Origin 并注入 cookie。**WKWebView 的自定义协议拦不到 WebSocket 升级**,页面 JS 也无法给 WebSocket 附加请求头——两条 Electron 的路都断了。

**Tauri 的等价机制**(`ws_bridge.rs`):壳在 loopback 起一个随机端口的监听,页面的 `streamBaseUrl` 指向它(boot 应答里下发);中继接受页面 WS 后:

1. 校验握手 Origin 必须是 `dsh-app://app`(其余 403);
2. 自行构造带完整握手头(`Sec-WebSocket-Key` 等)的升级请求连向 Host,注入 Host 信任的 `Origin` 与 cookie;
3. 全双工转发帧,直到任一侧关闭。

目的:保持"凭据只在壳里"与"Web 客户端零改动"(客户端 `remoteStreamUrl()` 本就支持 `streamBaseUrl` 重定向)。**强度差距**(README 已记录):Electron 绑定内核可信的 `webContentsId`,本桥只验证可伪造的 Origin + loopback 随机端口。对"防本机其他进程"弱一档,对"防恶意网页跨站连 Host"等价(浏览器会带上真实 Origin)。

### 2.5 Host 进程监护

`host.rs` 与 `host-process.ts` 的对照:

| 职责 | Electron | Tauri |
|---|---|---|
| 启动参数 | `node --expose-internals <entry> runtimeDir profileDir primaryRuntime link pnpm nodeBin` | **完全一致的 argv**(`host.rs` `supervise()`) |
| 环境 | `DSH_HOME`、`DSH_DESKTOP_NODE_EXECUTABLE`、PATH 前置 node-bin | 同一组,外加 `DSH_DESKTOP_HOST_CONTROL=stdio` |
| 日志/诊断 | stdout 透传;stderr 保留末 64Ki | 同(stdout 前缀透传,stderr 环形缓冲 64Ki) |
| 生命周期 | ready/fatal/退出上报 | 同(socket 事件 + EOF 后 wait) |
| 停机 | shutdown → 10s → SIGTERM → 5s → SIGKILL | 同(`stop_blocking()`) |

目的:Host 的启动契约完全不变,`apps/desktop-host` 的 argv 与消息协议一个字节没动;变的只是传输载体。退出路径挂在 `RunEvent::ExitRequested` 上,保证关窗前先排干 Host。

### 2.6 页面桥:preload ↔ 初始化脚本

初始化脚本(`main.rs` `init_script()`)在页面任何脚本之前运行,暴露的全集与 Electron preload 一一对应:

| 全局 | Electron preload | Tauri 初始化脚本 |
|---|---|---|
| `dshDesktopBoot.ready/failed` | `ipcRenderer.invoke` | `internals.invoke('desktop_boot'/'desktop_boot_failed')` |
| `dshDesktop` | 类型化产品 API(更新) | 协议版本桩 `{ protocolVersion: 1 }`(更新 UI 因此隐藏,客户端把它当"无更新桥") |
| `data-platform` | `markDocumentPlatform()` | 直接 `setAttribute`(平台值由 Rust 注入,macos→darwin 映射) |
| `__DSH_DIRECTORY_PICKER__.pick` | IPC + 原生对话框 | `desktop_pick_directory` 命令 → `tauri-plugin-dialog` 原生选择器 |
| 主题观察(macOS) | `syncNativeTheme()` | MutationObserver + `desktop_set_theme` |

`desktop_boot` 的应答内容与 Electron 相同:`{ injections, streamBaseUrl }`——注入表来自 Host ready 事件原样透传,`streamBaseUrl` 是流桥 origin。页面 `main.ts` 的启动序(等门 → 设 `__DSH_TRANSPORT__` → 逐条应用注入 → 放行 SPA)**零改动**。

### 2.7 原生集成:用维护中的生态件

| 能力 | 方案 | 替代的 Electron 内建 |
|---|---|---|
| 致命错误对话框 | `tauri-plugin-dialog` 原生 message box(页面覆盖层同时保留) | `dialog.showMessageBox` |
| 目录选择器 | 同插件的 folder picker,挂主窗口 | `dialog.showOpenDialog` |
| 单实例/profile 锁 | `tauri-plugin-single-instance`,二次启动聚焦已有窗口 | `requestSingleInstanceLock` |
| Edit 菜单与快捷键 | macOS 上 Tauri 默认菜单自带 | 手工 `Menu.buildFromTemplate` |

目的:这些不自己造。致命路径的展示是"页面覆盖层 + 原生对话框"双通道(`state.rs` `show_fatal()`),由 Host 监视线程安全触发。

### 2.8 开发工作流

```sh
pnpm run dev:desktop-tauri    # 构建仓库 → 复用 apps/desktop 的准备产物 → tauri dev
pnpm run start:desktop-tauri  # 产物已备好,直接再启动
```

`scripts/dev.ts` 做三件事:调 `apps/desktop/scripts` 的 `prepareDevelopmentProject` 与 `preparePrimaryRuntime`(一次性工程、捆绑运行时,与 Electron dev **共享同一份产物**);用 `DesktopProjectManager.applyRelease(false)` 初始化**隔离的** `home-tauri` Harness home(不碰 Electron 的 dev home 和用户 `~/.dsh`);带 `DSH_TAURI_REPO_ROOT` 启动 `tauri dev`。

注意:Electron 版是**应用启动时**做 profile 初始化(`main.ts` 的 `backend.start` 回调),Tauri 原型把它放在开发启动器里——这是已记录的差距(打包模式必须挪回壳内)。

---

## 3. 安全设计对照

威胁模型与 Electron 版 §3 完全一致,逐场景对照防线:

| 攻击场景 | Electron 防线 | Tauri 原型防线 |
|---|---|---|
| XSS 想读宿主 cookie | 主进程内存持有 | Rust 内存持有(§2.3) |
| XSS 直接调系统能力 | 无原始 IPC + sender 校验 | 页面只有 4 个窄命令(boot/pick/theme/上报失败),无文件/进程/参数面 |
| 被攻陷页面开新窗口钓鱼 | `setWindowOpenHandler` deny | WebView 默认无 window.open 通路,`on_navigation` 兜底 |
| 页面被诱导导航恶意源 | `will-navigate` 白名单 | `navigation_allowed()`:`dsh-app:` + loopback http(s) |
| 静态资源路径穿越 | resolve 后目录前缀校验 | 组件级拒绝 `..`(§2.2) |
| 宿主借响应种 cookie | 转发剥 `set-cookie` | 同 |
| 恶意网页跨站连 Host WS | 主进程按 webContentsId 改写 | 流桥 Origin 门禁(§2.4,强度差距已记录) |
| 本地运行时被篡改 | SHA-256 清单逐文件校验 | **未实现**(打包差距) |
| 两实例竞争 profile | 进程级单实例锁 | single-instance 插件 |

IPC 面收敛说明:Tauri 的 capability 文件声明页面可 invoke 的权限;本壳只注册自家命令,页面桥(初始化脚本)是唯一入口,生态插件的选择器/对话框都由 Rust 侧调用,不向页面开插件权限。

---

## 4. 设计决策速查

| # | 决策 | 理由 | 位置 |
|---|---|---|---|
| 1 | 薄壳,复用 Web 后端与传输 | 继承 Electron 版决策 | `2026-09-10` 笔记 |
| 2 | Host 用系统 Node + 完全相同的 argv | Tauri 无 Node;契约不变使 host 代码零改动 | `paths.rs`、`host.rs` |
| 3 | 控制通道 = 双工 socket + JSON 行(env 门控 stdio 传输) | 不手写 Node IPC 帧;Electron 路径零变化 | `desktop-host/src/control.ts` |
| 4 | 凭据只在 Rust(转发注入 + WS 中继注入) | 渲染不可信假设 | `web_document.rs`、`ws_bridge.rs` |
| 5 | WS 走壳内 loopback 中继 | WKWebView 拦不了 WS 升级,页面加不了头 | `ws_bridge.rs` |
| 6 | 原生集成全用 tauri 生态件 | 维护中的方案优先于手写 | Cargo.toml |
| 7 | macOS 透明开 `macos-private-api` | 毛玻璃的官方前置;与 tauri.conf 配对 | `tauri.conf.json` |
| 8 | dev home 隔离(`home-tauri`) | 与 Electron dev 并行不互踩 profile | `scripts/dev.ts` |
| 9 | MIME 取自解析后文件 | 根路径无扩展名,误用 octet-stream 会白屏 | `web_document.rs` |
| 10 | node 一律解析为绝对路径 | node-bin 启动器相对名会自递归 | `paths.rs` |

## 5. 关键文件索引(两侧对照)

| 职责 | Electron | Tauri |
|---|---|---|
| 壳入口/窗口/命令 | `apps/desktop/src/main.ts` | `src-tauri/src/main.rs` |
| 共享状态(启动事实/凭据/窗口) | main.ts 内闭包 | `src-tauri/src/state.rs` |
| 静态服务 + 认证转发 | `apps/desktop/src/web-document.ts` | `src-tauri/src/web_document.rs` |
| Host 监护 | `apps/desktop/src/host-process.ts` | `src-tauri/src/host.rs` |
| WS 流桥(新增) | main.ts 内改写 | `src-tauri/src/ws_bridge.rs` |
| 路径/运行时解析 | `main.ts` `runtimeResources()` | `src-tauri/src/paths.rs` |
| Host 控制传输(共享改动) | `process.send`(不变) | `apps/desktop-host/src/control.ts`(env 门控) |
| 开发启动器 | `apps/desktop/scripts/dev.ts` | `apps/desktop-tauri/scripts/dev.ts` |

## 6. 已知差距与后续路径

| 差距 | 后续路径 |
|---|---|
| 自动更新/强制更新/更新任务控制 | 生态路径 `tauri-plugin-updater` + `tauri-plugin-process`;需先有打包与签名,以及与 dsh 发布编排(先载荷后元数据)的对接 |
| 打包布局(内置 Node/pnpm、`desktop-runtime.json` 校验、签名公证) | 复用 `apps/desktop/scripts/prepare-*` 与 primary-runtime 的 Node;清单校验在 Rust 重写 |
| profile 初始化在启动器而非壳内 | 打包时挪入壳启动序 |
| 自定义应用菜单/About、Windows 标题栏 overlay | `tauri::menu` 成熟 API,纯工作量 |
| 红绿灯坐标 | 无公开 API;可评估 objc2 直接调用 |
| WS 桥按窗口绑定 | 需要壳级每连接票据(当前 URL 形态放不下 query,见 §2.4) |
| Windows/Linux | 控制通道 socket 与 dev 路径为 unix 专用,需移植 |

## 7. 踩坑记录(给后来者)

1. **node-bin 递归**:私有启动器 exec `$DSH_DESKTOP_NODE_EXECUTABLE`,相对名 + PATH 前置 = 自我递归。Node 必须先解析成绝对路径。
2. **MIME 白屏**:自定义协议的响应 MIME 取错了源(URL 路径而非文件),WKWebView 静默拒绝渲染,表象是"协议返回 200 但页面不再发请求"。诊断手段:在协议处理器打点请求日志,观察"只有 index 没有后续资源请求"。
3. **单实例 + 孤儿进程会污染调试**:杀外层 shell 不一定杀掉 app 子进程;残留实例持锁后,新实例静默退出,日志会误导排查方向。测试循环里要按 pgrep 清场。
4. **`macos-private-api` 双声明**:Cargo feature 与 `tauri.conf.json` 的 `macOSPrivateApi` 缺一个,构建脚本直接失败;这是 Tauri 防止发布版意外依赖私有 API 的闸门。
5. **tokio 不收阻塞 socket**:std 绑定的 listener 交给 tokio 前必须 `set_nonblocking`,或直接用 `tokio::net::TcpListener::bind().await`(本壳采用后者)。
6. **tungstenite 自定义升级请求要自带握手头**:`connect_async(Request)` 不会替你补 `Sec-WebSocket-Key/Version/Connection/Upgrade/Host`,缺了报 "Missing, duplicated or incorrect header"。
