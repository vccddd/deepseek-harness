# Desktop Host(桌面宿主进程)

[English](README.md) | 中文

`@deepseek-ai/dsh-desktop-host` 是桌面壳拉起的私有 Node 模式宿主进程,负责运行共享 Web 应用后端。它自身不含产品逻辑:引导 desktop profile、向壳回报认证后的应用 URL,然后在 loopback HTTP 与 WebSocket 上服务页面。两个载体以完全相同的方式拉起它——Electron 壳(`apps/desktop`)与 Tauri 壳原型(`apps/desktop-tauri`)——两者说的都是本文规定的控制通道。

## 控制通道

Host 与拉起它的壳之间只说一套生命周期协议。传输由 `createDesktopHostControl`(`src/control.ts`)在进程启动时选择:

| 传输 | 选择条件 | 帧格式 |
|---|---|---|
| Electron IPC | 默认:环境中没有 `DSH_DESKTOP_HOST_CONTROL` | Node IPC 通道(fd 3 上的 V8 序列化) |
| 双工 socket | `DSH_DESKTOP_HOST_CONTROL=stdio` | 换行分隔的 JSON:命令经 stdin(fd 0)到达,事件写入 `DSH_DESKTOP_HOST_CONTROL_FD`(默认 `0`)所指描述符的写方向 |

stdio 传输为非 Electron 壳而设。Tauri 壳把 Unix socketpair 的一端作为 Host 的 stdin 并从同一 socket 读取事件;未设置该环境变量时 Electron 路径逐字节不变。两种传输投递同一消息集——`tests/control.spec.ts` 把同一命令流分别跑过真实 IPC 通道(fork 出的 `tests/control-ipc-fixture.ts`)与 stdio 通道对,断言投递一致。

## 命令(壳 → Host)

| 帧 | 字段 | 含义 |
|---|---|---|
| `{"type":"shutdown"}` | — | 请求停机。Host 停掉 profile、发送 `shutdown-complete` 后退出。 |
| `{"type":"update-tasks","requestId":N,"action":"inspect"\|"lock"\|"unlock"}` | `requestId` 是壳分配的安全整数,应答原样回带;`action` 读取活跃任务、获取更新准入锁或释放它 | 供壳的更新流程使用的更新任务控制。 |

`shutdown` 无应答时壳逐级升级:Electron 壳等 10 秒、发 SIGTERM、再等 5 秒、最后 SIGKILL(`apps/desktop/src/host-process.ts` 的 `stop()`);Tauri 壳执行同一梯度(`apps/desktop-tauri/src-tauri/src/host.rs` 的 `stop_blocking()`)。`update-tasks` 命令无应答时在壳的控制请求期限处失败(Electron 壳为 10 秒);缺失的应答永远不会授权安装。

## 事件(Host → 壳)

| 帧 | 字段 | 含义 |
|---|---|---|
| `{"type":"ready","url":"...","injections":[...]}` | `url` 是启动令牌认证 URL,壳用它兑换自己持有的 cookie;`injections` 是索引注入表(`IndexInjection` 值),原样转发进 boot 握手 | profile 引导完成后恰好发送一次。 |
| `{"type":"fatal","message":"..."}` | — | 启动失败。Host 随后以退出码 1 退出。 |
| `{"type":"shutdown-complete"}` | — | 对 `shutdown` 的应答:profile 进程树已停止。未经请求的 `shutdown-complete` 是协议违规。 |
| `{"type":"update-tasks","requestId":N,"active":bool,"error":"..."?}` | 应答按 `requestId` 与命令配对。`error` 存在表示请求被拒(例如 Host 正在停机);`active` 说明是否有活跃任务会受影响 | — |

## 错误语义

Host 一侧:无法解析的 JSON 行被静默丢弃,可解析但不是合法命令的帧被静默忽略(`parseDesktopHostCommand`)。该通道假定壳可信:只校验帧形状,不防御恶意输入。壳一侧:形状校验失败的事件帧是致命的——Electron 壳让 Host 失败并发送 SIGTERM;Tauri 监护把无效控制帧当致命错误处理。通道 EOF 或 IPC 断开意味着壳已消失:Host 不等 `shutdown`,自行拆除。

## 进程契约

Host 以 `node --expose-internals <entry> runtimeDir projectDir primaryRuntime resolution pnpm nodeBin` 的形式被拉起(`src/index.ts`;两个壳传完全一致的 argv)。`ready` 是健康通道上的第一个事件且恰好发送一次。环境为 profile home 设置 `DSH_HOME`,stdio 传输下另设 `DSH_DESKTOP_HOST_CONTROL` 与 `DSH_DESKTOP_HOST_CONTROL_FD`。

## 版本治理

协议帧内不携带版本字段。兼容性由 `DESKTOP_HOST_PROTOCOL_VERSION`(`apps/desktop/src/host-protocol.ts`,当前为 `4`)治理:它以 `hostProtocolVersion` 记录在 `desktop-runtime.json` 中,并在发布清单加载时与壳的期望值核对(`apps/desktop/src/release.ts`)。对消息集、字段含义或上文错误语义的任何变更,必须在同一 PR 中提升该常量,并同时更新两个壳的消费端、Host 与一致性测试。

## 包布局

- `src/control.ts` — 传输选择与两种传输的实现(面向壳与测试的公开面)。
- `src/index.ts` — Host 入口:profile 引导、ready 上报、命令处理。
- `src/update-tasks.ts`、`src/office.ts`、`src/primary-runtime.ts` — 通道背后的 Host 侧行为。
- `tests/control.spec.ts`、`tests/control-ipc-fixture.ts` — 协议与传输一致性测试。
