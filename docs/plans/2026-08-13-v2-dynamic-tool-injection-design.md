# 智脑 v2 动态工具注入设计

| 项 | 值 |
|---|---|
| 日期 | 2026-08-13 |
| 版本 | v1.0 |
| 状态 | 设计已确认，待 `writing-plans` |
| 范围 | v2 MainBrain、工具执行、MCP、Plugins、Skills、WebSearch、Evolver、Motor Brain |
| 兼容策略 | 保留现有工具名和输入 Schema，内部迁移为动态 Provider |

## 1. 背景与目标

当前 Web/CLI 的 v2 MainBrain 在 `create_v2_main_brain` 中调用
`mvp_tool_definitions()`，把一组固定工具一次性写入 `MainBrain.tools`。定义发现、
执行路由、权限描述和 Motor Brain 能力表彼此分离，导致以下问题：

1. `mvp_tool_specs()` 是静态全集，主脑会看到后端并不存在的工具。
2. MCP 配置虽然被读取，生产链路却创建空的 `McpClientPool`；连接、发现和调用仍是桩实现。
3. `LSP`、`ListMcpResources`、`ReadMcpResource`、`McpAuth`、`RemoteTrigger`、
   `MCP`、`TestingPermission` 会返回空数据或假成功。
4. 插件工具可以在另一条 CLI 路径聚合，但没有进入 v2 MainBrain。
5. `ToolSearch` 只搜索静态延迟工具表，无法反映 MCP、插件或会话工具的实时状态。
6. `MainBrain`、`RealToolExecutor`、`brain-motor` 和 `brain-eval` 各自维护工具清单，
   名称、权限和实际能力会漂移。
7. Evolver 使用始终返回空结果的 `StubWebSearch`，与主脑联网能力割裂。
8. Windows 上若没有 `HOME`，v2 会错误回退到 `/tmp`，从而漏掉用户目录中的
   MCP、插件和技能配置。

本设计的目标是建立一条 Codex 风格的运行时扩展链：工具由后端 Provider 发现，
注册中心生成一致快照，主脑只广告当前可执行的能力，调用再由同一快照路由。
MCP 至少支持本地 stdio 和远程 Streamable HTTP，并允许在配置或连接状态变化后刷新。

## 2. 已确认的设计决策

1. 采用兼容方案 A：继续支持 `WebSearch`、`read_file` 等现有名称和输入参数。
2. 建立一个统一动态注册中心，不给现有静态列表简单套壳。
3. Builtin、MCP、Plugin、Session 四类 Provider 使用同一注册协议。
4. 工具定义、执行句柄、权限元数据和来源信息必须在同一条注册记录中提交。
5. 后端不可用的工具不注册；生产环境不再广告任何桩能力。
6. MCP 原生工具名采用 `mcp__{server}__{tool}`。
7. 插件原生工具名采用 `plugin__{plugin}__{tool}`；旧名仅作为显式兼容别名。
8. 配置或连接变化可热刷新；进程无需重启。
9. 已开始的请求固定使用一个注册表快照，新快照只影响之后的请求。
10. WebSearch 保留稳定门面，实际搜索后端通过 Provider 动态选择。

## 3. 方案比较

### 3.1 方案一：统一 ToolProvider + ToolRegistry（采用）

所有工具后端实现统一 Provider 协议，将完整注册记录发布到一个版本化注册中心。
MainBrain、ToolSearch、Motor Brain、Evolver 和权限层都读取该中心。

优点：定义和执行不会漂移，能真实热更新，MCP/插件/会话工具天然可注入。
代价：需要调整多个 crate 的边界，并迁移旧静态入口。

### 3.2 方案二：保留静态表，仅追加 MCP/插件定义（不采用）

在 `mvp_tool_specs()` 外部拼接 MCP 和插件工具，并让执行器增加几个分支。

优点：改动较少。缺点：仍存在多套清单、静态 ToolSearch、假工具和权限漂移，
无法真正满足“全部动态注入”。

### 3.3 方案三：所有工具全部外置到 MCP（不采用）

文件、Shell、记忆等基础工具也改成 MCP 服务。

优点：扩展协议统一。缺点：基础能力增加进程和网络故障面、延迟及部署成本，
还会破坏当前工作目录和权限语义。

## 4. 总体架构

```text
 BuiltinProvider ─┐
 McpProvider ─────┼── discover/refresh ──> DynamicToolRegistry
 PluginProvider ──┤                         ├─ ToolSnapshot(version N)
 SessionProvider ─┘                         │  ├─ definitions
                                            │  ├─ routes
                                            │  └─ capabilities
                                            ├─ ToolSearch
                                            ├─ MainBrain / EvalBrain
                                            ├─ MotorBrain / Guard
                                            └─ Evolver SearchCapability
```

注册中心是唯一真相源。Provider 不直接修改 MainBrain，也不单独维护一份可见工具数组。

### 4.1 crate 边界

- `brain-core`
  - 定义 `DynamicToolRegistry` 所需的核心类型和对象安全协议：
    `ToolProvider`、`ToolRegistration`、`ToolMetadata`、`ToolSnapshot`、
    `ToolRoute`、`ToolRegistryError`。
  - 扩展现有 `ToolDescriptor`，但不依赖具体后端 crate。
- `brain-motor`
  - 实现线程安全、版本化的 `DynamicToolRegistry`。
  - 删除自己的内置工具硬编码表和 stub 执行，改为读取共享快照并委托执行句柄。
- `tools`
  - 实现 `BuiltinProvider` 以及内置工具的注册记录。
  - 保留工具具体逻辑；`mvp_tool_specs()` 降为过渡兼容入口，生产 v2 不再调用。
- `brain-mcp`
  - 实现 `McpProvider`、连接状态和真实的工具/资源调用。
  - 复用并收敛 `runtime` 中已经通过测试的 JSON-RPC/stdio 能力，避免两套 MCP 客户端继续并存。
- `plugins`
  - 实现 `PluginProvider`，把已启用插件的工具、权限和执行句柄交给统一注册中心。
- `brain-main`
  - 保存 `Arc<dyn ToolRegistryView>`，每次请求开始时获取一个快照。
  - 工具循环接收快照，而不是独立的 `Vec<ToolDefinition>`。
- `ai-brain-cli`
  - 仅负责组装 Provider、启动/关闭生命周期和暴露刷新命令，不再拼接工具列表。
- `brain-evolver`、`brain-eval`
  - 从共享注册表构建受策略约束的快照；不再自建 WebSearch stub 或定义表。

依赖方向必须保持从抽象到实现单向流动，禁止为了复用注册中心而让 `brain-core`
反向依赖 `tools`、`brain-mcp` 或 `plugins`。

## 5. 核心数据模型

### 5.1 ToolRegistration

每条注册记录至少包含：

- `canonical_name`：注册中心唯一名称。
- `aliases`：显式兼容别名。
- `definition`：LLM 使用的名称、描述和 JSON Schema。
- `provider_id`、`source_kind`、`source_instance`：来源与诊断信息。
- `permission`：只读、工作区写入或危险操作。
- `risk_level`、`side_effecting`、`network_access`、`requires_confirmation`。
- `availability`：后端健康状态摘要。
- `search_terms` / `scenarios`：供 ToolSearch 与 Motor Brain 使用。
- `executor`：对象安全的异步执行句柄。

定义和 executor 必须原子提交，禁止只注册 Definition 而没有可调用后端。

### 5.2 ToolSnapshot

`ToolSnapshot` 是不可变对象，包含：

- 单调递增的 `version`。
- 已排序的工具注册记录。
- canonical name 与 alias 的路由索引。
- 生成 LLM `ToolDefinition` 的投影。
- Provider 状态、拒绝项和冲突诊断。

注册中心通过短临界区替换 `Arc<ToolSnapshot>`。获取快照不得等待网络、进程或磁盘 I/O。
不强制引入 `arc-swap`；若标准库 `RwLock<Arc<_>>` 足以通过并发测试，则优先避免新依赖。

### 5.3 请求一致性

每次 `process_input` / `process_input_streaming` / 协作成员 fork 在开始执行时只获取一次快照。
同一个工具循环的所有 LLM 请求、允许调用校验和实际路由都使用这个快照。

这保证：

- Provider 在请求途中刷新时，不会出现“模型刚看到工具、执行时却已被撤销”的竞态。
- 新请求能立即看到新版本。
- 正在运行的 MCP 调用可正常结束；Provider 关闭旧连接时等待旧快照释放或执行完成。

会话级附加工具不修改全局快照。它通过 `SessionProvider` 创建派生请求快照，并使用相同的
冲突、权限和执行规则。

## 6. Provider 生命周期与热更新

### 6.1 启动

1. 使用统一路径解析器确定用户配置根和项目配置根。
2. 创建空注册中心。
3. 同步发现本地 Builtin、内存、图谱、Skill 等不需要外部握手的 Provider。
4. 启动 MCP 和插件发现任务。
5. 每个 Provider 成功发现后独立发布候选注册记录，注册中心构建新快照。
6. 单个 Provider 失败只记录诊断，不阻止其他能力和 MainBrain 启动。

主脑可以在外部 Provider 尚未连接时先工作，但不会看见尚不可执行的工具。

### 6.2 刷新

刷新来源包括：

- MCP 连接、断线和重连。
- 插件启用、禁用、安装或卸载。
- Skills、MCP 和插件配置变化。
- 显式管理命令/API 调用。

首版热更新采用“显式 reload + 有界周期检测”的可移植实现；配置指纹变化才重建 Provider。
若后续采用文件系统 watcher，只替换触发机制，不改变 Provider/Registry 合同。

刷新采用两阶段提交：先在注册中心之外完成解析、握手和冲突校验，只有完整候选集合有效时
才替换该 Provider 的上一版记录。配置暂时写到一半或 MCP 短暂失败时，不能把整个全局快照
清空；失败 Provider 保留明确状态，并按策略决定保留上一份健康快照或撤销已经确定失效的能力。

### 6.3 关闭

Orchestrator 关闭时按 Session → Plugin → MCP → Builtin 的逆序调用 Provider shutdown。
MCP 子进程必须收到关闭请求并被有界等待；超时后记录错误并终止由智脑启动的子进程。

## 7. 名称、别名与冲突

### 7.1 命名规则

- 内置兼容工具：继续使用现有名字，例如 `read_file`、`WebSearch`。
- MCP：`mcp__{normalized_server}__{normalized_tool}`。
- Plugin：`plugin__{normalized_plugin}__{normalized_tool}`。
- Session：使用调用方声明的限定名；协作工具等现有兼容名作为显式 alias。

服务名和工具名必须经过确定性的安全编码，避免 `__`、空白或大小写折叠造成二义性；
原始名称保存在元数据中，调用 MCP 时再映射回原名。

### 7.2 冲突规则

1. canonical name 冲突默认拒绝，不允许静默覆盖。
2. alias 冲突默认拒绝该 alias，但不必撤销双方的 canonical name。
3. 内置兼容别名只有配置明确选择 Provider 时才能改绑，例如 `WebSearch` 的后端选择。
4. 配置可声明唯一的 `preferred_provider` 或显式 alias 绑定；不能依赖发现顺序决定胜负。
5. 所有拒绝项进入中文诊断并可通过状态接口查询。

## 8. 工具可见性与 ToolSearch

注册记录包含 `exposure`：

- `base`：每次请求直接广告的核心能力。
- `deferred`：由 ToolSearch 查询后加入该请求的派生快照。
- `internal`：只能被系统组件调用，不直接广告给 LLM。

`ToolSearch` 本身是 base 工具，其查询对象改为当前请求快照中的 deferred 记录，返回完整匹配
元数据和可选择名称。选择工具后，只扩展当前请求的广告集合，执行路由仍来自同一快照。

因此 MCP 或插件新工具在刷新后可被下一次请求立即发现，不再由
`deferred_tool_specs()` 的静态数组决定。

## 9. MCP 设计

### 9.1 配置模型

保留现有 `mcpServers` JSON 兼容，同时把内部模型统一到一个解析结果。优先级为：

1. 用户级 `~/.ai-brain/mcp/mcp-servers.json`。
2. 项目级 `.ai-brain/mcp/mcp-servers.json`。
3. 已启用插件声明的 MCP 配置。
4. 机器本地 override。

同名服务器按明确 scope 优先级合并并记录来源，不能依赖 HashMap 迭代顺序。

支持字段：

- stdio：`command`、`args`、`env`、启动和调用超时。
- Streamable HTTP：`url`、静态 headers、从环境变量读取的 headers/Bearer Token。
- OAuth：client 配置、授权状态和凭据引用。
- enable/disable、工具 allow/deny、刷新间隔。

敏感信息不写入工具描述或普通日志；环境变量只保存变量名，凭据持久化到专用凭据存储。

### 9.2 stdio

复用 `runtime::McpServerManager` 已有的 initialize、分页 `tools/list`、`tools/call`、
`resources/list`、`resources/read`、超时、重试和进程退出检测能力。将其移入或封装在
`brain-mcp` 的真实客户端边界，v2 不再创建空池。

协议客户端优先采用仓库已经依赖的官方 Rust SDK `rmcp`，开启 client、child-process、
Streamable HTTP 和 auth 所需 feature；`runtime::McpServerManager` 中现有稳定 fixture 和
错误处理作为迁移基线。只有官方 SDK 未覆盖且测试证明必要的部分才保留自研传输，避免继续
同时维护两套完整 MCP 协议栈。实施时必须评估并升级当前锁定的 `rmcp 1.7`，以覆盖目标协议版本。

### 9.3 Streamable HTTP

在 `brain-mcp` 增加符合 MCP JSON-RPC 会话语义的 HTTP transport：

- 按协商版本执行生命周期：2025 系列兼容 initialize 和可选 `Mcp-Session-Id`；
  2026-07-28 及之后使用无会话请求、`server/discover` 和每请求 `_meta`。
- 支持普通 JSON 响应及服务器要求的流式响应。
- 传播协商协议版本、`MCP-Protocol-Version`、`Mcp-Method`、`Mcp-Name`、认证头、
  超时和取消；请求头与 JSON-RPC body 必须一致。
- 正确处理非 2xx、协议错误、超时、断线和会话失效。
- 与 stdio 共用工具分页、资源分页、调用结果解析和命名逻辑。

工具列表热更新也必须按协议版本适配：旧版连接在服务声明 `tools.listChanged` 后消费
`notifications/tools/list_changed`；2026-07-28 及之后通过 `subscriptions/listen`
订阅工具变化。收到通知后重新执行完整分页 `tools/list`，校验成功后一次性替换该服务的
注册记录。若通知流异常关闭，则重订阅并重新拉取，不能靠旧缓存永久运行。

旧 SSE 配置只作为迁移输入；若没有可验证实现则标记不支持并不给工具，不用假连接。

### 9.4 资源与认证兼容工具

- `ListMcpResources`：至少一个已连接 MCP 服务支持 resources 时动态注册，调用真实分页列表。
- `ReadMcpResource`：同条件注册，调用真实 `resources/read`。
- `McpAuth`：只有服务需要且当前 UI/CLI 可以完成授权时注册；它返回真实授权状态或启动真实流程。
- `MCP`：不再作为生产通用代理广告。现有调用若必须兼容，可作为 internal alias 路由到
  已发现的 `mcp__server__tool`，但不得返回假结果。

连接断开时，对应 MCP 原生工具在下一版快照撤销；资源门面则按剩余可用服务器重新计算。

## 10. Builtin 与假工具迁移

| 现有能力 | 迁移结果 |
|---|---|
| 文件、Shell、REPL、Notebook、计划、结构化输出 | 由 BuiltinProvider 动态发布，保留名称和 Schema |
| `Skill` | 仅当 SkillCatalog 有可用技能时发布；执行继续读取真实技能内容 |
| `Agent` | 仅当 Agent runtime/dispatch 可用时发布 |
| `AskUserQuestion` | 保留真实 tool-loop 用户响应通道，不按底层辅助函数误判为 stub |
| 记忆工具 | 仅当 PyramidMemoryBrain 可用时发布 |
| 图谱工具 | 仅当图数据库可打开且对应后端可用时发布 |
| `LSP` | 仅在真实 Language Server Provider 初始化成功时发布；本次不伪造 LSP 客户端 |
| MCP 工具/资源 | 由 McpProvider 真实发现和路由 |
| `RemoteTrigger` | 仅存在显式允许的端点 Provider 时发布；执行真实 HTTP 请求并校验状态码 |
| `TestingPermission` | 只在测试配置/测试构建中由 SessionProvider 注入，生产永不发布 |
| 停用的 novel 工具 | 保持停用，不因动态迁移意外恢复 |

`RemoteTrigger` 不能接受模型任意提交未配置 URL 作为默认能力。配置必须声明允许的 endpoint、
方法、认证引用、超时和网络策略；实际调用仍经过危险操作确认和 SSRF/重定向边界检查。

## 11. WebSearch 动态后端

### 11.1 稳定门面

`WebSearch` 的外部名称和输入 Schema 不变。BuiltinProvider 根据配置和健康状态把它绑定到
一个 `SearchBackend`：

- `builtin_ddg`：现有 DuckDuckGo HTML 搜索，作为本地兼容后备。
- `mcp:<server>/<tool>`：例如 Open WebSearch、SearXNG 等 MCP 工具。
- `http:<provider>`：明确配置的搜索 HTTP 后端。

默认优先级由配置决定，不能因为 MCP 发现顺序变化而漂移。只有后端成功初始化时才发布
`WebSearch`；全部不可用时不广告该工具，并输出诊断。

### 11.2 健康切换

- 配置可开启有界 fallback 链。
- 只有网络错误、超时、5xx 或明确限流等可重试错误才能切换。
- 参数错误、权限拒绝和安全拒绝不能偷偷换后端。
- 一次调用的后端尝试、耗时和失败原因进入结构化诊断，不泄露密钥。

### 11.3 内置 HTTP 修复

内置 WebSearch/WebFetch 必须：

- 校验 HTTP 状态码后再解析正文。
- 使用显式连接/总超时、重定向上限和响应大小上限。
- 正确尊重系统代理及 `NO_PROXY`/`no_proxy`，测试不得依赖开发机代理状态。
- 保留 `CLAWD_WEB_SEARCH_BASE_URL` 兼容，但把 Provider URL 正式纳入配置。
- WebFetch 返回清晰的内容类型、最终 URL 和截断信息；不宣称支持 JavaScript 或反爬。

### 11.4 Evolver

Evolver 不再构造 `StubWebSearch`。它获得注册中心提供的 `SearchCapability` 受限视图：

- 有 `WebSearch` 时调用同一门面和同一后端策略。
- 没有时返回明确的 capability unavailable，而不是成功空列表。
- Evolver 的网络权限和调用预算仍可独立收紧。

## 12. Plugins 与 Skills

### 12.1 Plugins

统一当前两套插件发现能力，生产 v2 使用 `plugins::PluginManager/PluginRegistry` 的启用状态、
校验、生命周期、工具聚合和权限信息；旧 `brain-plugin::PluginManager` 只保留迁移兼容或最终删除。

PluginProvider 对每个启用插件：

1. 校验 manifest 和命令路径。
2. 创建限定 canonical name。
3. 将插件声明的权限映射到统一 ToolMetadata。
4. 提交定义与真实执行句柄。
5. 插件禁用或失效时撤销其工具并执行 shutdown。

插件原始未限定名称默认不抢占内置工具。需要保留旧名时必须在配置中声明 alias。

### 12.2 Skills

Skills 是 prompt/工作流扩展，不等同于可执行工具 Provider。动态行为分两层：

- SkillCatalog 从用户、项目、Codex 兼容目录和已启用插件目录构建不可变快照。
- `Skill` 工具及 system prompt 摘要从同一 CatalogSnapshot 产生。

刷新后，新请求读取新技能快照；已运行请求保持旧快照。扫描失败不能清空上一份健康 Catalog。

## 13. MainBrain、EvalBrain 与协作运行

### 13.1 MainBrain

移除生产初始化中的 `mvp_tool_definitions()` 和 `register_tools(tool_defs)`。MainBrain 保存注册表视图，
处理请求时获取快照并把其 definitions 传给 tool loop。

`register_tools` 仅保留测试/过渡用途，内部实现为构建一个 SessionProvider 或静态测试快照，
不再成为生产工具真相源。

### 13.2 tool loop

tool loop 接收 `Arc<ToolSnapshot>`：

- 构建每轮 LLM request 时使用快照的当前广告集合。
- 拒绝不在该请求广告集合中的工具调用。
- 权限检查读取对应注册记录的 metadata，再叠加参数级 guard 和 hooks。
- 执行直接调用快照内 route，避免 Definition 与 `RealToolExecutor` 二次查表漂移。
- `AskUserQuestion` 继续保留交互特殊通道，但它也必须来自快照中的真实注册记录。

### 13.3 EvalBrain

删除手写 `build_read_only_tool_definitions()`。评估脑从全局快照派生只读 PolicyView：

- 仅选择明确标记只读的注册记录。
- `bash` 仍叠加只读命令策略，不能仅因工具 metadata 是危险级而整体丢失现有验证能力。
- Skill 只在 Catalog 可用时出现。

### 13.4 协作成员与会话工具

协作成员 fork 在请求开始时从全局快照派生 SessionProvider 快照，加入
`read_group_messages` 等请求专属工具。`allow_tools=false` 生成空的广告视图，不能修改全局注册表。

## 14. Motor Brain 与权限

Motor Brain 删除 `with_builtin_tools()`、关键词到固定工具名的映射和 `execute_stub()`：

- 可用工具、描述、场景标签和风险全部读取共享快照。
- 快速选择通过注册记录的 `search_terms/scenarios` 匹配，不写死 `Read/Edit/LSP`。
- 执行委托快照 route；无 route 的记录不可能进入快照。
- 未知工具默认拒绝和高风险，不能像当前 `guard_check` 一样默认放行。

权限顺序为：

1. 请求广告集合校验。
2. 注册 metadata 的基础权限与风险策略。
3. 参数级 guard（危险 shell、敏感路径、远程 URL 等）。
4. PreToolUse hook。
5. Provider 执行。
6. PostToolUse / failure hook。

动态注入不能绕开现有工作目录、sandbox 或用户确认。MCP 注解可作为风险提示输入，
但不能降低本地配置设定的最低风险等级；未知的外部工具默认按危险操作处理。

## 15. 路径与配置发现

新增唯一的 `AiBrainPaths` 解析器，供 Orchestrator、Skills、Plugins、MCP、图谱和凭据使用：

1. 若配置显式设置 `AI_BRAIN_HOME`，使用该目录。
2. 否则使用平台 home API；兼容 `HOME` 和 Windows `USERPROFILE`。
3. 无法确定用户目录时返回明确错误或使用显式项目目录，禁止静默写入 `/tmp` 或进程当前目录。

项目级配置从请求/服务确定的 workspace root 解析，不通过全局 `set_current_dir` 推导。
`.Codex.json` 与 `.Codex/settings.local.json` 继续遵循仓库约定；工具运行时配置读取保持既有兼容，
本设计不自动覆盖这些文件。

## 16. 错误处理与可观测性

注册中心提供状态快照：

- 当前 registry version 和工具总数。
- 每个 Provider 的状态、最后成功刷新时间和错误摘要。
- 工具来源、canonical name、aliases、exposure 和风险。
- 冲突、配置错误、撤销和后端切换记录。

日志优先使用中文且结构化，禁止打印 token、Authorization、完整敏感 headers 或 OAuth code。

错误分类：

- `Unavailable`：能力当前不可用，可由模型选择其他工具。
- `InvalidInput`：Schema/参数错误，不重试、不切换后端。
- `PermissionDenied`：权限或安全策略拒绝。
- `Timeout` / `Transport`：可按 Provider 策略重试或切换。
- `Protocol`：MCP/插件协议响应无效。
- `Backend`：真实后端返回失败。

工具失败必须设置 `is_error=true`，不允许用 HTTP 200、空数组或“triggered”文本伪装成功。

## 17. 测试设计

实施采用 TDD，每项先写失败测试。至少覆盖：

### 17.1 注册中心

- Provider 注册、替换、撤销和版本递增。
- 定义与执行 route 原子一致。
- canonical/alias 冲突和显式优先级。
- base/deferred/internal 可见性。
- 并发读取与刷新不死锁、不暴露半成品。
- 已开始请求固定旧快照，新请求获得新快照。

### 17.2 MainBrain 与会话

- v2 初始化不再调用静态全集。
- 同一工具循环广告和执行使用同一快照。
- `ToolSearch` 动态选择 deferred MCP/插件工具。
- streaming、上下文压缩重入、协作 fork 均保持请求快照。
- `allow_tools=false` 和 SessionProvider 隔离。

### 17.3 MCP

- 真实 stdio fixture：initialize、分页工具发现、工具调用、资源列举/读取、超时和重启。
- 本地 HTTP fixture：2025 生命周期与 session header、2026-07-28 无会话生命周期、
  JSON/流式响应、标准协议头、工具调用、资源和非 2xx。
- 两代工具变更机制：旧版 `notifications/tools/list_changed` 与新版
  `subscriptions/listen` 均能触发完整重发现和原子刷新。
- headers、Bearer 环境引用、OAuth 状态和凭据脱敏。
- 断线撤销、重连重新注入、一个服务器失败不影响其他服务器。
- 原始名称与 `mcp__server__tool` 编解码往返。

### 17.4 Plugins 与 Skills

- 启用/禁用插件动态增加和撤销工具。
- 插件权限映射、命令失败、重复名和 alias 冲突。
- Skills 用户/项目/插件目录刷新，失败保留健康快照。
- Windows `USERPROFILE` 与显式 `AI_BRAIN_HOME`。

### 17.5 WebSearch 与 Evolver

- DDG/HTTP/MCP 后端选择和有界 fallback。
- 非 2xx、超时、重定向、响应大小、内容类型。
- 系统代理开启且 localhost 命中 `NO_PROXY` 时测试稳定。
- Evolver 使用真实 SearchCapability；无能力时明确失败而非空成功。

### 17.6 假工具清理与权限

- 生产快照不包含 `TestingPermission`。
- 无真实 LSP/MCP/Auth/RemoteTrigger 后端时，相应工具不可见。
- `AskUserQuestion` 仍能真实等待并接收用户响应。
- 未知动态工具默认拒绝；危险 MCP/插件/RemoteTrigger 需要确认。
- 静态扫描不再存在生产可达的“not yet implemented”“stub response”成功路径。

### 17.7 最终门禁

从 `rust/` 执行：

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

另需构建 release、启动 v2 Web 服务并做真实冒烟：

1. 内置工具调用。
2. 当前用户配置中的 `open-websearch` stdio MCP 被发现并成功调用。
3. 插件工具启停后无需重启即可反映。
4. MCP 配置刷新和断线重连。
5. WebSearch 在代理开启的 Windows 环境正常工作。
6. 状态接口显示来源、版本和真实健康状态，且无敏感信息。

## 18. 迁移顺序与回滚

实施按以下顺序降低风险：

1. 引入核心注册类型和并发快照，保留旧生产入口。
2. 将内置工具转换为 BuiltinProvider，建立定义/执行一致性测试。
3. MainBrain、tool loop、协作运行切换到请求快照。
4. ToolSearch、Motor Brain、EvalBrain 切换到共享视图。
5. 用真实 stdio MCP 替换 `McpClientPool` stub，再增加 Streamable HTTP。
6. 接入 PluginProvider 和 Skills 快照。
7. WebSearch Provider 化并让 Evolver 复用。
8. 条件接入 LSP/RemoteTrigger，移除所有生产 stub 和旧静态入口。
9. 全量门禁、release 构建和真实 v2 冒烟。

每个阶段保留适配器让测试可逐步迁移，但适配器不得成为新的真相源。回滚时可以把 MainBrain
暂时切回上一版 Provider 快照；不回滚到广告假工具的静态全集。

## 19. 完成定义

只有同时满足以下条件才算“全部修复”：

1. v2 生产路径不存在固定注册全部工具的调用。
2. MainBrain 广告的每个工具都有同快照内的真实执行 route。
3. MCP 配置会真实连接、发现并调用 stdio/Streamable HTTP 服务。
4. 插件工具、技能和会话工具能动态进入下一次请求。
5. ToolSearch、Motor Brain、EvalBrain、Evolver 不再依赖自己的静态工具真相源。
6. 所有桩工具从生产可见面消失或被真实 Provider 替代。
7. Windows 用户目录和代理场景有自动化测试与真实冒烟证据。
8. 权限、sandbox、hooks 和敏感信息保护对动态工具同样有效。
9. Rust 规定的格式、Clippy、工作区测试及 release 冒烟全部有新鲜验证结果。
