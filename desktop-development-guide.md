# DeepSeek Harness 桌面端开发指南:工程化与安全设计解读

> 基于发布 `dsh-v0.1.6-alpha.2` 的源码与官方决策笔记整理。面向**非 Electron 背景**的开发者:不需要事先了解 Electron,只需具备一般的 Web 与 Node.js 知识。文中每个设计都附带"为什么这么设计"的理由,理由来源分两类:代码事实(标注文件路径)与官方决策记录(标注 `.agents/notes/` 下的笔记路径)。

---

## 0. 五分钟 Electron 背景

读后面的内容只需要这四个概念:

| 概念 | 是什么 | 类比 |
|---|---|---|
| **主进程 (main)** | 唯一的 Node.js 进程,拥有窗口、菜单、文件系统、子进程等全部系统能力 | 操作系统的"内核侧" |
| **渲染进程 (renderer)** | 一个 Chromium 浏览器标签页,运行产品 UI(HTML/JS/CSS),默认**没有任何**系统能力 | 普通网页 |
| **preload** | 夹在两者之间的一小段受信脚本,是渲染页面唯一能"够到"主进程的窄通道 | 网页里的 `postMessage` 桥 |
| **IPC** | 主进程与渲染进程之间的进程间通信通道 | 自定义的 `window.postMessage` |

关键安全事实:渲染进程跑的是完整 Web 应用,和浏览器里的页面一样**可能被 XSS 攻陷**。所以桌面端安全设计的核心问题只有一个——**假设页面被攻陷,攻击者还能拿到什么?** 后文的安全部分就是对这个问题的系统回答。

---

## 1. 总体架构:薄壳 (thin-wrapper)

### 1.1 三个进程

```
┌─────────────────────────────────────────────────────────────┐
│ Electron 主进程 (apps/desktop)                               │
│  窗口/菜单/对话框 · dsh-app:// 协议 · 自动更新 · 签名 · 恢复    │
└──────────┬──────────────────────────────┬───────────────────┘
           │ stdio IPC (生命周期消息)        │ HTTP+WS 代理转发
           ▼                              │
┌─────────────────────────┐               │
│ Desktop Host 子进程      │◄──────────────┘
│ (apps/desktop-host,     │   127.0.0.1:19387 (loopback)
│  ELECTRON_RUN_AS_NODE)  │
│  共享 Web profile runner │
│  + agent 运行时          │
└─────────────────────────┘
           ▲ 加载 (dsh-app://app/)
┌──────────┴────────────────┐
│ 渲染进程 = 共享 Web SPA     │
│ (apps/web 构建产物,        │
│  与浏览器版完全同一份)      │
└───────────────────────────┘
```

四个应用各司其职:

| 应用 | 职责 |
|---|---|
| `apps/desktop` | Electron 壳:窗口、原生菜单、自定义协议、自动更新、签名、故障恢复。**不含任何产品逻辑** |
| `apps/desktop-host` | 私有 Node 宿主进程,引导共享 profile runner 起 agent 后端,再挂两个桌面专属插件(office 运行时、更新任务控制) |
| `apps/web` | Web 客户端 SPA 的 Vite 构建,`dist/` 同时供浏览器版和桌面版使用 |
| `apps/cli` | `dsh` CLI,提供所有界面共享的 profile 启动逻辑 |

### 1.2 为什么是"薄壳"而不是独立桌面应用

**官方理由**(决策笔记 `2026-09-10-desktop-web-wrapper.zh.md`):如果为桌面端维护第二套后端组合与传输,那么每一条 Web 路由、认证变化、流式能力、配置重载行为,桌面端都要重复实现或明确省略——**必然遗漏功能**。桌面端真正的差异化需求只有:独立安装、原生窗口控件、离线运行时。这些由壳负责,其余全部继承 Web。

具体放弃了哪些替代方案(均来自决策记录):

| 放弃的方案 | 放弃理由 |
|---|---|
| 第二套后端组合 + 私有分帧字节管道 | 避免监听端口的收益,抵不过重复实现 Web 全部服务行为;分帧管道还会有 Base64 膨胀与跨版本 V8 序列化问题 |
| 把产品 UI 永久打包进 Electron(与后端分离更新) | 需要新的版本兼容计划;GUI 协议本来就绑定客户端与后端版本 |
| Electron 专用插件管理页 | 重复包操作、IPC、本地化、重启处理;原生恢复已覆盖 Host 起不来的场景 |
| 固定 registry/store、只允许批准的插件来源 | 同一用户配置在桌面与 Web 行为不一致;隔离靠"独立安装归属"已足够,不需要来源限制 |

### 1.3 发布身份:一个版本号绑定一切

壳 (Electron)、dsh 运行时、Web 客户端、私有 Host 打包为**一个发布单元**,版本号完全一致——即使壳代码没改,dsh 升级也必须产生新的桌面发布(`2026-08-25-electron-desktop-packaging-and-updates.zh.md`)。

**为什么**:如果各组件独立定版本,会产生"未经验证的壳 × 客户端 × 后端 × 插件"组合,且无法明确判断更新是否可用。版本绑定把组合爆炸问题变成发布流程问题。

---

## 2. 工程化产出详解

### 2.1 启动流程:先见窗口,再见后端

桌面端采用"立即显示窗口"策略(`2026-09-09-desktop-immediate-window-and-direct-start.zh.md`):

1. 主进程注册特权协议 `dsh-app://`,创建窗口并**立即**从 `app.asar` 内加载打包好的 Web 静态资源(共享加载页)——此时后端还没启动;
2. 并行拉起 Desktop Host 子进程(stdio IPC,协议版本常量 `DESKTOP_HOST_PROTOCOL_VERSION = 4`,见 `apps/desktop/src/host-protocol.ts`);
3. Host 就绪后回报认证 URL 与启动注入表,主进程用 URL 换取认证 cookie(见 §3.4);
4. 渲染页通过 preload 的 `dshDesktopBoot.ready()` 拿到注入表,设置 `__DSH_TRANSPORT__`,放行 `__DSH_BOOT_READY__` 门,SPA 正常启动。

**为什么**:用户点击图标到看见界面的时间与后端启动解耦;且 Host 启动失败时,加载页与原生恢复对话框仍然可用,不会白屏。

### 2.2 传输:复用 Web 的 HTTP+WebSocket

桌面端不发明新 RPC。Host 在 loopback `127.0.0.1:19387` 起与 Web 版完全相同的服务(浏览器版用 3080),RPC 走 HTTP POST `/api`,流式数据走 `/api/remote.mux` WebSocket。Electron 做的只是"适配浏览器安全模型"(详见 §3.4)。

**为什么**:认证、流式、重载、路由全部由 Web 实现一份,桌面端自动继承全部行为(`2026-09-10` 笔记)。

### 2.3 运行时自足:不依赖系统 Node/pnpm/Python

这里有一次值得学习的**决策反转**:最初设计内置独立的上游 Node.js 可执行文件,后来改为直接用 Electron 自己当 Node 运行时(`2026-09-11-desktop-electron-node-runtime.zh.md`)。

| 决策 | 理由 |
|---|---|
| Host 跑在 `ELECTRON_RUN_AS_NODE=1` 的 Electron 下 | 不再重复分发一份 JS 运行时(省一份二进制、下载、签名、版本选择);代价是接受 Electron 的 Node 补丁与原生 ABI 作为发布兼容性责任 |
| 启动加 `--expose-internals` | Cordis 加载器需要 Node 内部 ESM 加载器,其 builtin 访问器在 Electron 44 中找不到所需符号;显式参数解决,无需改 Cordis |
| 内置固定版本 pnpm | 应用必须在无系统 pnpm 的机器上装插件;包操作遵循用户正常 pnpm 配置 |
| 捆绑 Python/Node/pnpm "主运行时" | office 等技能需要 numpy/pandas 等;按 `primary-runtime-lock.json` 哈希锁定下载,离线装到 `~/.dsh/dsh-runtimes/`,不污染用户环境 |
| 自带 `runtime/bin` 只注入包安装进程 | PTC 和 agent shell 的 PATH 保持用户原样,防止内部启动器被劫持解析 |

### 2.4 Profile 与状态归属:桌面与 CLI 互不干涉

- Electron 以进程级单实例锁独占 `$DSH_HOME/profiles/desktop`;CLI 保留并拒绝 `desktop` 这个 profile 名(`apps/cli/src/args.ts:74`);
- 桌面 profile 用 **Web 模板**的 bundle 初始化(保证插件行为与 Web 一致),再加桌面专属插件;
- 打包版启动时清理 profile 里与运行时清单重复的核心包副本。

**为什么独立安装**(2026-08-25 笔记):桌面与 CLI 共享产品数据(会话、设置、凭据),但**绝不共享**可执行包、lockfile、`node_modules`、插件激活状态——任何一方都可能改掉另一方的 Cordis 版本、插件版本或原生模块,产生不可复现的故障。共享数据靠各数据 owner 的格式版本与进程锁保护。

### 2.5 平台适配

| 平台 | 适配内容 |
|---|---|
| macOS | `hiddenInset` 标题栏 + 侧栏 vibrancy 毛玻璃;显式补回 ⌘W/⌘M/⌘H 标准菜单;DMG+ZIP 双产物、公证、钉票 |
| Windows | 40-DIP 原生标题栏 overlay,颜色随应用调色板经 IPC 同步;菜单改为渲染器驱动的原生弹出菜单,编辑命令转合成按键;NSIS 自定义安装器(免管理员权限) |
| Linux | 非发布目标,仅保留开发可用性(如目录选择器无 zenity/kdialog 时自动降级为浏览模式) |

UI 层的平台分支靠 preload 给 `<html>` 打的 `data-platform` 属性,客户端 8 个包的 CSS 用它做平台分支(如 macOS 下侧栏为红绿灯按钮让位,`packages/client/ui-layout/src/client/AppFrame.module.css:76`);探测原语 `isDarwinDesktop()` 特意注明"渲染时读取,标记可能晚到 DOMContentLoaded"(`packages/client/ui-primitives/src/darwin-desktop.ts`)。

目录选择是"原生能力替换"的典型:Web 用 Host 选择器;桌面用挂在应用窗口上的 Electron 原生对话框(并发请求合并为一个、取消可重试、窗口销毁丢弃结果);Linux 自动降级。`directory-picker-auto` 按环境探测四档选择。

### 2.6 构建、发布与更新

**开发工作流**:

```sh
pnpm run dev:desktop     # 构建 + 投影一次性 npm 工程 + 启动未打包 Electron
pnpm run start:desktop   # 不重新构建,用现有产物再启动
pnpm run package:desktop:mac:arm64   # 打包目标固定:mac-arm64 / mac-x64 / win-x64
```

`dev:desktop` 的隔离设计:开发 Harness home、一次性工程、Electron 用户数据全在 `apps/desktop/.desktop-build/` 下,不碰用户的 `~/.dsh`;调试端口 9229/9222/9230 可用环境变量覆盖。

**版本推导规则**(2026-09-16 笔记):生产发布用 dsh 的精确版本(含 alpha/beta/rc);测试发布在同一 base 上追加 `.YYYYMMDD.index`(稳定 base 用 `-test.` 前缀)。**为什么**:客户端只接受更高版本——如果测试版占了正式版本号,修正后的发布就无法自动分发给装了测试版的机器。测试后缀保证它永远排在下一个正式版本之前,自动降级被禁用。

**更新链路**:electron-updater generic provider,源强制 HTTPS,差分下载(blockmap,NSIS 把 blockmap 嵌进签名安装器,macOS ZIP 用独立 blockmap)。上传顺序被严格编排:**先上传全部不可变的版本化载荷与 blockmap,最后才替换频道元数据**——保证客户端永远不会读到指向不完整产物的元数据。上传前还要校验完成记录、dsh 根版本、桌面版本、产物名、大小、SHA-512 全部一致,才允许读取凭据发送数据。

**签名**:
- macOS:Developer ID + hardenedRuntime + 公证 + 钉票;ZIP 与 DMG 从两份隔离的 App 副本并行公证;签名后钩子跑 Apple 严格应用验证并要求叶证书 Authority 与 Team ID 精确匹配。
- Windows:SafeNet USB token 上的 EV 私钥(**不可导出**,SignTool 用 `/kc` 指定 key container 直接引用硬件密钥);签名串行执行、首次失败即停(防止排队任务重复提交错误 PIN 锁死 token);有监督式预检与硬件联锁文件;未签名模式仅限本地诊断,产物隔离且**不配置更新 feed**。
- 一个真实的工程细节:打包强制 `ELECTRON_BUILDER_7Z_FILTER=BCJ`,因为 7-Zip 默认为 ARM64 PE 自动选的过滤器会被 NSIS 解码器漏解两个 `node-pty` 二进制——这是用原生 NSIS 解压验证发现并锁定的。

---

## 3. 安全设计详解

### 3.1 威胁模型

一句话:**渲染进程(整个 Web UI)不可信**。它跑复杂的前端代码、渲染第三方内容、可能被 XSS;所以设计目标是——即使页面被完全攻陷,攻击者也拿不到宿主凭据、文件系统、shell、任意包管理操作,也无法伪造更新。

### 3.2 第一层:渲染器硬隔离

窗口配置(`apps/desktop/src/main.ts:134`):`nodeIntegration: false` + `contextIsolation: true` + `sandbox: true` + `webSecurity: true`——Electron 的全部最严格档。含义:页面没有 Node 能力、页面 JS 与 preload 隔离运行、渲染进程整体沙箱化。

preload 只暴露三组**类型化窄接口**(`apps/desktop/src/preload-app.ts`):目录选择、启动握手、更新状态查询。而且 preload 自带二次检查:只有 `location` 是 `dsh-app://app` 时才暴露完整桥,其他源拿到的是空壳对象。官方原则(2026-08-25 笔记):**任何渲染器都拿不到文件系统、原始 IPC、shell 命令或 pnpm 参数**。

### 3.3 第二层:IPC 面收敛

主进程每个 IPC handler 都有调用方校验,三层递进:

1. `assertDesktopSender`(`apps/desktop/src/ipc.ts:71`):sender frame 的 URL 必须是 `dsh-app:` 协议且 hostname 在允许列表;
2. `assertProductSender`(`main.ts:226`):进一步要求是**主窗口的主 frame**——同源 iframe 也调不了;
3. 个别通道(如 `bootFailed`)再加主窗口归属检查。

更新接口是"权限分离"的范本:产品页只能**查询状态**和**请求打开**——`open()` 在主进程弹原生确认框;版本号、包 URL、安装授权永远不进渲染器(`ipc.ts` 中 `DshDesktopProductApi` 的接口注释明确写了这一点)。状态里的诊断字段也标注"不含子进程输出或凭据"。

### 3.4 第三层:凭据只在主进程(传输安全)

宿主认证凭据是一条**只存在主进程内存里的 cookie**,页面从头到尾见不到:

```
页面请求 dsh-app://app/api/...
   │  (页面同源请求,天然不带宿主 cookie)
   ▼
主进程 protocol.handle
   ├─ 静态资源 → 从 app.asar 读,resolve 后必须在 dist 目录内(防路径穿越,403)
   └─ 其余 → forwardWebRequest (web-document.ts:59)
        ├─ origin 不是 dsh-app://app → 403
        ├─ 删除入站 host/origin/cookie/sec-fetch-site
        ├─ 代入主进程持有的宿主 cookie,转发到 127.0.0.1:19387
        └─ 响应剥离 content-encoding/content-length/set-cookie(宿主无法借道种 cookie)
```

WebSocket 同理:`onBeforeSendHeaders` 只对**主窗口 + 目标恰为宿主地址 + origin 为 `dsh-app://app`** 的连接改写 origin 并附加 cookie,其余直接 cancel(`main.ts:431`)。

净效果:即使 UI 被 XSS,攻击者(1)拿不到宿主 cookie;(2)页面的源是 `dsh-app://`,不是宿主源,浏览器跨源保护照常生效;(3)打不开新窗口去钓鱼(见下)。

### 3.5 第四层:导航与弹窗管控

- `setWindowOpenHandler` 一律 deny,http/https 链接交给系统浏览器打开(`main.ts:142`);
- `will-navigate` 只允许 `dsh-app:` 内部导航或同 origin 的 http(宿主重定向),其余拦截并外开(`main.ts:173`);
- 右键菜单只暴露编辑角色,没有"检查元素"等入口。

### 3.6 第五层:更新链路防投毒

- 更新源强制 HTTPS(配置解析里 `httpsOrigin()` 包死,`desktop-auto-update-environment.mjs:138`);
- 平台原生验签:electron-updater 在 Windows 校验安装器 Authenticode 签名与打包记录的发布者,macOS 校验更新包代码签名;下载状态机里有独立的 `verifying` 阶段;
- 壳 + dsh 运行时 + pnpm 是**一个签名更新单元**,杜绝"壳不动只换运行时"的组合攻击面;
- 上传编排先载荷后元数据、全量一致性校验后才动凭据(见 §2.6);
- 强制更新策略跑在独立原生窗口,渲染器无法伪造或绕过。

### 3.7 第六层:本地运行时完整性

`app.asar/dsh/desktop-runtime.json` 记录文件级清单(路径、字节数、SHA-256、可执行位)。启动时 `verifyDesktopRuntime`(`apps/desktop/src/runtime-tree.ts:188`)逐项核对:schema 版本、平台/架构、Electron 版本与发布版本一致、共享包 name/version 与磁盘 manifest 一致、文件清单精确比对——任何一项不符直接拒绝启动。捆绑 Python/Node 分发同样由 `primary-runtime-lock.json` 的哈希锁定。

**为什么**:更新单元把大量可执行代码放进用户目录,清单校验让"被篡改的本地运行时"无法静默运行。

### 3.8 第七层:状态与信息卫生

- 单实例锁 + 独占 profile,防两个进程竞争改包状态;
- 恢复流程**不解析**用户的 `cordis.patch.yml`,直接改名备份——畸形输入不参与逻辑;
- 诊断信息有界:原生对话框最多 1200 个 UTF-16 码元,Host stderr 只保留最后 64Ki 字符,既防泄漏也防膨胀;
- Windows 签名 PIN 在一切对外诊断中被替换;签名字段在 SignTool 启动前从环境中清除。

### 3.9 攻击场景 × 防线对照表

| 攻击场景 | 被哪层挡住 |
|---|---|
| Web UI 被 XSS,想读宿主 cookie | §3.4:cookie 只在主进程内存 |
| XSS 想直接调 IPC 干系统能力 | §3.2/§3.3:没有原始 IPC,窄接口 + sender 校验 |
| 被攻陷页面开新窗口钓鱼 | §3.5:窗口打开一律 deny |
| 页面被诱导导航到恶意源 | §3.5:will-navigate 白名单 |
| 静态资源路径穿越读任意文件 | §3.4:resolve 后目录前缀校验 |
| 宿主(或中间人)借响应种 cookie | §3.4:转发剥离 set-cookie |
| 更新源被投毒 | §3.6:HTTPS + 平台验签 + 上传编排 |
| 本地运行时被篡改 | §3.7:SHA-256 清单逐文件校验 |
| 两个实例竞争破坏 profile | §3.8:进程级单实例锁 |

---

## 4. 设计决策速查

| # | 决策 | 理由 | 决策记录 |
|---|---|---|---|
| 1 | 薄壳:复用 Web 后端与传输 | 第二套后端必然遗漏 Web 功能 | `architecture/2026-09-10-desktop-web-wrapper` |
| 2 | 发布身份绑定(壳=dsh=Host 同版本) | 避免未经验证的组件组合 | `architecture/2026-08-25-electron-desktop-packaging-and-updates` |
| 3 | Electron RunAsNode 替代独立 Node | 不重复分发运行时;接受 Electron ABI 作为发布责任 | `architecture/2026-09-11-desktop-electron-node-runtime` |
| 4 | 桌面独占 profile,CLI 拒绝 | 防互改依赖图 | `architecture/2026-08-25` |
| 5 | 内置 pnpm + 捆绑 Python/Node 运行时 | 无系统依赖可用;哈希锁定防篡改 | `feature/2026-09-14-desktop-primary-runtime` |
| 6 | 凭据只在主进程(认证 cookie 代理) | 渲染器不可信假设 | 代码:`web-document.ts`、`main.ts:431` |
| 7 | 更新先载荷后元数据 | 元数据永远不指向不完整发布 | `architecture/2026-08-25` |
| 8 | EV 私钥不导出,硬件引用 | 私钥不可离开 token | `architecture/2026-08-25` |
| 9 | 测试版本加日期后缀 | 客户端只升不降,防占正式版本号 | `process/2026-09-16-desktop-release-version-derivation` |
| 10 | 原生恢复不解析用户 YAML | 畸形输入不参与恢复逻辑 | `architecture/2026-09-15-desktop-native-fatal-recovery` |

## 5. 关键文件索引

| 位置 | 职责 |
|---|---|
| `apps/desktop/src/main.ts` | 主进程:窗口、协议、IPC、导航管控、WS 改写、更新协调 |
| `apps/desktop/src/web-document.ts` | 静态资源服务 + 宿主认证 + 请求转发 |
| `apps/desktop/src/ipc.ts` | IPC 通道表、类型化产品 API、sender 校验 |
| `apps/desktop/src/preload-app.ts` | 渲染器可见的全部桥 |
| `apps/desktop/src/host-process.ts` | Host 子进程生命周期 |
| `apps/desktop/src/runtime-tree.ts` | `desktop-runtime.json` 清单与完整性校验 |
| `apps/desktop/scripts/electron-builder-config.mjs` | 打包配置工厂(签名、公证、NSIS) |
| `apps/desktop/scripts/desktop-auto-update-environment.mjs` | 更新源解析(强制 HTTPS) |
| `apps/desktop-host/src/` | 私有 Host 入口 + office 运行时 + 更新任务控制 |
| `.agents/notes/implemented/{architecture,feature,process}/2026-*-desktop-*.zh.md` | 官方决策记录(中英双语) |

## 6. 给 review 者的检查清单

改动桌面端代码时,按这条主线自检:

1. **渲染器可见的东西变多了吗?** preload 每加一个接口都是攻击面,需要 sender 校验与类型化协议;
2. **凭据路径变短了吗?** 任何让 cookie/token/路径进入渲染进程的改动都违反核心假设;
3. **导航/弹窗白名单还闭合吗?** 新的协议、窗口、frame 都要过 `setWindowOpenHandler`/`will-navigate`/sender 校验;
4. **更新单元还完整吗?** 壳与 dsh 版本仍需一致;上传顺序仍是载荷先行;
5. **桌面与 Web 行为还一致吗?** 共享能力改桌面侧,Web 侧同步;反之亦然;
6. **文案走类型化字典了吗?** 壳层中英文案受 `verify-client-ui-i18n` 门禁约束。
