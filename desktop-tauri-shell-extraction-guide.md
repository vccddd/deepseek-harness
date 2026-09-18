# 把 Tauri 壳抽成可独立引入的"真壳":评估记录(已否决)

> **决策(2026-09-18)**:不做抽取。`apps/desktop-tauri` 作为与 `apps/desktop`(Electron)并行同仓演进的第二载体,不独立发布、不跨仓引入——即下文 §5 的选项 C,且不推进 Phase 2。本文保留为评估记录:§1 的边界清单、§4 的两条线协议、§7 的债务清单对**并行维护**仍然有效;§6 中 Phase 0 的两项(控制通道协议文档化、两种传输的一致性测试)有独立价值,**已于 2026-09-18 落地**:协议文档在 `apps/desktop-host/README.md`(消息表、错误语义、`DESKTOP_HOST_PROTOCOL_VERSION` 版本治理),一致性测试在 `apps/desktop-host/tests/control.spec.ts`(同一命令流跑真实 IPC 通道与 stdio 通道对,断言投递一致);§7 的 `SHELL: OnceLock` 全局态也已消除(state 改经 Tauri manager 管理)。其余计划不再执行。
>
> 目标读者:接手 `apps/desktop-tauri` 后续演进的 Agent。前置阅读:[desktop-development-guide.md](desktop-development-guide.md)(Electron 版设计解读)、[desktop-tauri-development-guide.md](desktop-tauri-development-guide.md)(当前 Tauri 原型实现解读)。本文原回答一个问题:**这个壳能不能从产品仓库里抽出来,像 git submodule 或 npm 依赖那样被引入,成为一个产品无关的"真正的壳"?** 评估结论曾为:能,但正确形态不是 submodule 也不是纯 npm,而是"Rust crate(Tauri 插件)+ 仓内一个薄应用",并且有一个必须先解决的身份绑定矛盾(§5)——该矛盾正是最终选择并行而非抽取的原因。

---

## 1. 先弄清楚"壳"指什么:边界清单

当前原型里,通用机制与 dsh 产品契约混在一起。抽取的第一步是把它们分开。逐模块盘点(代码现状以 `apps/desktop-tauri/src-tauri/src/` 为准):

| 模块 | 通用机制(可抽) | 产品契约(留在产品侧) | 现状硬编码点 |
|---|---|---|---|
| `main.rs` 窗口 | 建窗、init script 注入、导航白名单骨架 | 窗口标题/尺寸、`dsh-app` scheme 名、允许的 loopback 来源 | 全部写死在 setup |
| `web_document.rs` | 静态白名单服务、BOOT 注入、路径穿越防护、认证转发、头剥离集合 | scheme 名、dist 根、注入的 boot 脚本文本 | `APP_ORIGIN` 常量、`state.paths.dist` |
| `ws_bridge.rs` | loopback 中继、Origin 门禁、握手头注入、帧转发 | 期望的页面 Origin、Host mux 路径 | `crate::APP_ORIGIN`、`/api/remote.mux` 默认值 |
| `host.rs` | 进程监护、分级停机、stderr 环形缓冲、ready→换 cookie | **Host 的 argv 模板与环境变量**、消息里的注入表语义 | `supervise()` 里的整段 argv、`DSH_HOME` 等 env |
| `paths.rs` | (无) | 全部:dev 根、profile、primary-runtime、pnpm、node 解析 | 整个模块是 dsh dev 布局 |
| `state.rs` | 启动状态机、boot 应答组装 | 应答字段(`injections`、`streamBaseUrl`) | 结构即契约 |
| `desktop-host/src/control.ts` | JSON 行控制传输 | 消息集合本身(ready/fatal/shutdown/update-tasks) | env 门控已隔离,归产品 |

**判定标准**:一句话描述——"任何『静态 Web dist + loopback 认证 Host + 页面桥』架构的产品都需要的东西"是壳;"只在 dsh 里有意义的名字、路径、参数"是产品配置。按这个标准,壳大约占现有代码的七成。

## 2. 引入方式选项对比

用户提出的两个方向(submodule、npm)与 Tauri 生态原生形态(crate/插件)逐一评估:

| 方式 | 能交付什么 | 优点 | 致命问题 |
|---|---|---|---|
| **git submodule** | 完整源码(crate + 脚本 + 文档) | 引入即得全部;不发布也能用 | 无版本语义(钉 commit,无 SemVer/变更日志);升级体验差;**CI/门禁双仓割裂**(壳的测试、lint 在哪跑?);本仓库 `vendor/` 政策明确是给**第三方上游**钉源码用的,自己的移动代码走 submodule 是反模式 |
| **npm module** | 只有 JS/TS 部分 | 生态熟悉 | **npm 装不来 Rust**:Tauri 壳的本体是 Rust crate,npm 包顶多带初始化脚本和 dev 工具的 TS;消费者仍要改自己的 Cargo.toml。纯 npm 引入在 Tauri 世界不成立 |
| **Rust crate(发布)** | 壳本体全部机制 | 版本语义完备;Tauri 插件体系的标准载体 | 引入方需要 Rust 工具链(本来就需要);需要把 §1 的产品契约做成配置 API |
| **Tauri 插件(crate + 可选 npm 绑定)** | crate + 少量 JS 胶水 | 同上,且初始化脚本/命令注册有官方挂载点(`tauri-plugin` 的 builder);对话/单实例等生态件可声明为插件的依赖 | 同上 |
| **仓内 workspace 包(不发布)** | 抽包但同仓同版本 | 零发布成本;版本绑定天然保持(见 §5) | 不满足"跨仓引入"的字面需求 |

**推荐**:两步走。先做**仓内抽包**(Phase 1,把通用机制抽成 crate + 产品薄层,验证边界切得对);抽稳后再决定是否发布为 **Tauri 插件**(Phase 2,`cargo add` + 可选 `npm i` 绑定包,这才是"submodule/npm 一样引入"的成熟等价物)。**明确否决 submodule**。

一个先例供参考:本仓库 `vendor/` 已有"钉 SHA 拉第三方源码"的成熟流程,但那是给不归我们管的上游用的;第一方代码的跨仓共享在本仓库目前没有先例,抽包即开创先例,需要按 §8 评估工装。

## 3. "真壳"的形状:配置面(产品侧要提供什么)

抽取后,产品侧(薄应用)只应提供一份声明式配置 + 一个 main。建议的配置面(名字可再议):

```rust
// 产品薄应用的全部内容(示意)
ShellConfig {
    scheme: "dsh-app",                    // 页面源
    document_root: resolve_dist(),        // 静态资源根(dev: 解析;打包: 资源内)
    host: HostLauncher {
        node: resolve_node(),             // 绝对路径(见踩坑记录 §7-1)
        argv: HOST_ARGV_TEMPLATE,          // 与 Electron 版完全一致的 8 个参数
        env: DSH_DESKTOP_ENV,              // DSH_HOME、PATH 前置等
    },
    boot: BootBridge {
        page_globals: "dshDesktopBoot/dshDesktop/__DSH_DIRECTORY_PICKER__/data-platform",
        theme_attribute: Some("data-ds-theme-source"),   // None = 不做主题镜像
    },
    navigation: NavigationPolicy { allowed_http: LoopbackOnly },
}
```

壳侧保证:协议转发带凭据、WS 中继、Host 监护与分级停机、单实例、boot 应答 `{ injections, streamBaseUrl }` 的组装、致命展示。**`paths.rs` 整体迁到产品侧**,它是纯 dsh dev 布局。

## 4. 壳 ↔ 产品的线协议:已经有一个,把它扶正

壳与产品之间真正需要长期稳定的是两条协议,而不是代码:

1. **控制通道 JSON 行协议**(壳 ↔ Host):`ready/fatal/shutdown-complete/update-tasks` 进,`shutdown/update-tasks` 出。现在由 `apps/desktop-host/src/control.ts` 实现 Host 侧,协议文档在 `apps/desktop-host/README.md`(消息表、字段、错误语义、传输选择)。抽取若重启,需把它归入 `DESKTOP_HOST_PROTOCOL_VERSION = 4` 的版本治理——壳实现和 Host 实现各自声明遵守的版本,不匹配时拒绝启动(当前版本经 `desktop-runtime.json` 清单核对,不在通道内握手)。这是"壳可独立演进"的前提。
2. **boot 应答契约**(壳 ↔ 页面):`{ injections, streamBaseUrl }` 加注入表语义(归 `IndexInjection` 体系)。这条已经稳定,文档化即可。

## 5. 核心矛盾:发布身份绑定 vs 独立引入(必须先回答)

Electron 版有一条明确的决策(2026-08-25):**壳、dsh 运行时、Web 客户端、Host 打包为一个发布单元,版本号完全一致**——理由是避免"未经验证的壳 × 客户端 × 后端 × 插件"组合,并把组合爆炸变成发布流程问题。

把壳抽成**独立版本**的引入物,直接与这条决策冲突:壳 1.2 × dsh 1.3 的组合没人验证过。三种调和方式:

| 方式 | 含义 | 代价 |
|---|---|---|
| A. 锁死同版本 | 壳 crate 的发布版本永远与 dsh 同步发,消费者只能用精确版本(类似 peerDependency 语义) | 发布流程强耦合;但与现决策完全一致 |
| B. 契约版本解耦 | 只有 §4 的两条协议是兼容面,壳按协议版本(SemVer)独立发 | 需要对协议做真正的向后兼容测试矩阵;改变了既有决策,需要新的决策记录 |
| C. 仓内包,不发布 | 抽包但同仓,永远同版本 | 不满足跨仓引入字面需求,但风险最低 |

**建议从 C 开始(Phase 1),以 A 为发布形态(Phase 2),B 只在出现第二个消费者且有专人维护协议测试时再考虑。**这是需要产品负责人拍板的第一个问题(§10)。

## 6. 分阶段落地计划

### Phase 0:契约固化(纯文档 + 测试,不动结构)
- [x] 写控制通道协议文档(消息表、字段、错误语义、`DESKTOP_HOST_PROTOCOL_VERSION` 版本治理)。落地:`apps/desktop-host/README.md`。
- [x] 给 `control.ts` 的 stdio 传输与 IPC 传输补齐"协议一致性"测试(同一组消息双向断言)。落地:`apps/desktop-host/tests/control.spec.ts` + `tests/control-ipc-fixture.ts`(fork 真实 IPC 通道)。
- [ ] 把 `APP_ORIGIN`、argv 模板、dist 根、env 集合整理成一张"产品契约清单"放进本文件 §3 对应位置(属抽取形态的配置面设计,抽取已否决,仅在重启时再做)。

### Phase 1:仓内抽包(重构,行为不变)
- [ ] 新建 Rust crate(建议 `apps/desktop-tauri/shell/` 或 `packages/desktop/shell/`,布局讨论见 §8),把 `web_document.rs / ws_bridge.rs / host.rs / state.rs` 的通用部分迁入,产品细节改为 `ShellConfig` 注入。
- [ ] `main.rs` 缩成"配置 + 建窗"的薄层,`paths.rs` 留在薄层。
- [ ] 每一步跑端到端启动验证(2 条 WS ESTABLISHED 为通过线),复用 `desktop-tauri-development-guide.md` §7 的清场纪律(杀进程按 pgrep,别信 TaskStop)。
- [ ] 行为不变的验收:页面零改动、Host argv 零改动、`desktop_boot` 应答字节级等价。

### Phase 2:发布形态(决策后)
- [ ] 按 §5 选定 A 或 B;A 则接入现有发布编排(先载荷后元数据的上传顺序约束一并继承)。
- [ ] crate 发布 + 可选 npm 绑定包(只含 init script 文本与 TS 类型)。
- [ ] 消费者文档:一个最小 main.rs 示例 + 配置面说明。

## 7. 抽取时要顺手解决的已知债务

- profile 初始化目前在 dev 启动器(`scripts/dev.ts`),打包前必须挪进壳启动序(对应 Electron `backend.start` 回调)——抽取时把"启动回调"做进 `ShellConfig`。
- ~~`main.rs` 的 `SHELL: OnceLock` 全局态改为随插件 state 管理,消除全局可变点。~~ 已消除:协议处理器经 `ctx.app_handle().try_state()` 取 `ShellState`,`app.manage()` 在建窗前完成。
- WS 桥的按窗口绑定(差距表):抽成通用件时把"连接门禁策略"做成 trait(Origin 检查是默认实现),给未来的强绑定留缝。
- 初始化脚本文本(`init_script()`)应成为壳的模板 + 产品提供全局名集合,而不是整段字符串都算产品配置。

## 8. 仓库工装影响(抽包前必须评估)

本仓库的包体系是纯 npm workspace(`@deepseek-ai/dsh-*`),**没有 Rust 包先例**。引入 crate 意味着:

- 门禁:`verify-application-entrypoints`、`hygiene`、`doc-sync`、`duplication` 均不认识 Cargo;需要决定 crate 放 `apps/`(应用私有,无发布)还是 `packages/`(包,受全部包门禁约束)。建议 Phase 1 放 `apps/desktop-tauri/shell/`(应用内 crate,零门禁冲击),Phase 2 发布时再讨论升级为 `packages/` 的新形态。
- CI:新增 Rust 构建矩阵(mac-arm64/mac-x64/win-x64)与缓存策略;`cargo vet`/`cargo audit` 是否纳入由安全侧定。
- `pnpm-workspace.yaml` 不需要动(crate 不是 npm 包);但如果发 npm 绑定包,`allowBuilds` 与 `minimumReleaseAge` 策略要过一遍。

## 9. 验证与验收(每个 Phase 的"完成"定义)

- Phase 0:协议文档评审通过;两条传输的一致性测试进 `pnpm run test`。
- Phase 1:端到端启动(2×WS)+ 既有门禁全绿 + `desktop_boot` 应答等价断言(可用 vitest 起 Host 对照,或加一个 Rust 集成测试比对 JSON)。
- Phase 2:示例消费者仓库按文档"cargo add + 配置"三分钟内跑起一个最小壳(静态页 + 假 Host)。

## 10. 开放问题(开工前找产品负责人确认)

1. §5 的版本策略选 A/B/C 哪个?这决定 Phase 2 是否存在。
2. 预期的第二个消费者是谁(另一个 dsh 产品?非 dsh 产品?)——若答案"没有",Phase 1 之后停止是合理结局。
3. 壳的更新链(目前留空)抽取后归谁:跟产品走(现状)还是壳自带 `tauri-plugin-updater` 接线?注意 Electron 版"壳+dsh 同一签名更新单元"的防投毒设计,抽壳后这条防线怎么保持。
4. Windows/Linux 支持时间点(控制通道 socket 是 unix 专用,抽取时是抽成 trait 还是先钉死 macOS)。

## 11. 明确不做

- 不用 git submodule 引入第一方代码(理由见 §2)。
- 不为"可引入"而把产品契约(认证 cookie 语义、Host argv、注入表)搬进壳——壳保持对 dsh 概念无知,只认配置与协议。
- 不在本轮顺手实现更新/打包(那是独立工作流,见 README 差距表)。
