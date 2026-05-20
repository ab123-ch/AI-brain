# 智脑原生 Skill/Plugin/MCP 系统设计

> 日期：2026-05-20
> 目标：让智脑原生支持 Skill 技能加载、插件管理、MCP 服务连接，适配现有技能市场和 MCP 服务生态

## 1. 设计决策

| 决策项 | 选择 | 理由 |
|--------|------|------|
| 架构方案 | 分层架构（B） | Skill/MCP/Plugin 生命周期差异大，分层最自然 |
| 设计范围 | Skill + MCP + Plugin（核心优先） | Agents/Hooks 已有基础，LSP/Monitors 后续迭代 |
| MCP SDK | rmcp crate | 官方 Rust SDK，成熟稳定 |
| 插件目录 | `~/.ai-brain/` | 独立目录，不依赖 Claude Code |
| 命名空间 | `plugin-name:skill-name` | 防止多插件间的 Skill 名称冲突 |

## 2. 整体架构

```
┌──────────────────────────────────────────┐
│           主脑 tool_loop                  │
│     通过 ToolProvider trait 调用          │
└──────┬──────────┬───────────┬────────────┘
       │          │           │
  ┌────▼──┐  ┌───▼────┐  ┌──▼───────┐
  │Skill  │  │Plugin  │  │MCP       │
  │Loader │  │Manager │  │ClientPool│
  └───────┘  └────────┘  └──────────┘
       │          │           │
  SKILL.md   plugin.json   rmcp
  文件系统    安装/卸载     JSON-RPC
```

三层独立，通过统一接口暴露给主脑：
- **SkillLoader**：扫描多个路径，解析 SKILL.md，管理命名空间
- **PluginManager**：管理 plugin.json 清单，安装/卸载/版本管理
- **McpClientPool**：基于 rmcp 管理多个 MCP 服务器连接，动态注册工具

### 新增 crate

```
rust/crates/
├── brain-plugin/     # 插件管理 + Skill 加载 + 注册表
└── brain-mcp/        # MCP 客户端连接池（基于 rmcp）
```

## 3. 目录结构

```
~/.ai-brain/
├── plugins/
│   ├── registry.json              # 全局插件注册表
│   └── cache/
│       └── {publisher}/{plugin}/
│           └── {version}/
│               ├── plugin.json    # 插件清单
│               ├── skills/        # 技能包
│               ├── agents/        # 子代理定义
│               └── hooks/         # 钩子配置
├── skills/                        # 独立安装的 Skill
│   └── {skill-name}/
│       └── SKILL.md
├── mcp/
│   └── mcp-servers.json           # MCP 服务器连接配置
└── data/
    └── mcp/                       # MCP 连接状态缓存
```

### Skill 搜索优先级（从高到低）

```
1. 项目级    {cwd}/.ai-brain/skills/{name}/SKILL.md
2. 用户插件  ~/.ai-brain/plugins/cache/{pub}/{plugin}/{ver}/skills/{name}/SKILL.md
3. 用户独立  ~/.ai-brain/skills/{name}/SKILL.md
4. 兼容层    ~/.claude/skills/{name}/SKILL.md
5. 兼容层    ~/.codex/skills/{name}/SKILL.md
```

## 4. SkillLoader 层

### SKILL.md 格式（兼容 Claude Code 规范）

```markdown
---
name: brainstorming
description: "强制头脑风暴，在实施前探索需求和设计。"
when_to_use: "当用户要求创建功能、构建组件时"
---

# Brainstorming
具体指令内容...
```

Frontmatter 解析规则：
- `name`：必填，kebab-case，必须匹配父目录名
- `description`：必填，LLM 据此判断是否激活
- `when_to_use`：可选，补充触发条件
- 其他字段忽略但保留，不报错

### 命名空间规则

| 来源 | 调用方式 | 说明 |
|------|---------|------|
| 插件内的 Skill | `plugin-name:skill-name` | 指定插件的 Skill |
| 独立安装的 Skill | `skill-name` | 用户级 Skill |
| 无冒号时 | 先查独立，再查插件 | 遍历所有插件的 skills 目录 |

### 三层渐进加载

```
启动时（元数据层）
  → 扫描所有 SKILL.md，只解析 name + description
  → 构建 skill_catalog: Vec<SkillMeta>（~100 tokens/skill）
  → 注入 system prompt: "<available_skills>..."

激活时（指令层）
  → LLM 调用 Skill("brainstorming") 或 Skill("superpowers:brainstorming")
  → 读取完整 SKILL.md body，返回给 LLM

资源层（按需，后续迭代）
  → scripts/、references/ 等附加文件
  → 当前不做，预留接口
```

### 关键数据结构

```rust
// brain-plugin/src/skill_loader.rs

struct SkillMeta {
    name: String,              // "brainstorming"
    namespace: Option<String>, // Some("superpowers") 或 None
    description: String,
    source_path: PathBuf,      // SKILL.md 的完整路径
}

struct SkillCatalog {
    skills: Vec<SkillMeta>,
}

impl SkillCatalog {
    fn scan_all(roots: &[PathBuf]) -> Result<Self>;
    fn resolve(&self, query: &str) -> Option<&SkillMeta>;
    fn load_content(&self, meta: &SkillMeta) -> Result<String>;
    fn summary_for_prompt(&self) -> String;  // <available_skills> XML
}
```

### 与现有代码的关系

- **替换** `tools/src/lib.rs` 中的 `resolve_skill_path()` + `parse_skill_description()`
- **替换** `commands/src/lib.rs` 中的 `discover_skill_roots()` + `parse_skill_frontmatter()`
- **替换** `brain-eval/src/skills/types.rs` 中的 `SkillRegistry`
- 统一为一套 SkillCatalog，主脑和评估脑共用

## 5. PluginManager 层

### plugin.json 清单格式

```json
{
  "name": "superpowers",
  "displayName": "Superpowers",
  "version": "4.3.1",
  "description": "Professional development skills for AI agents",
  "author": "obra",
  "skills": "./skills/",
  "mcpServers": "./mcp-config.json",
  "hooks": "./hooks.json"
}
```

字段说明：
- `name`：必填，kebab-case，用作命名空间前缀
- `displayName`/`version`/`description`/`author`：元数据
- `skills`：技能目录路径（相对于 plugin.json）
- `mcpServers`：MCP 服务器配置文件路径
- `hooks`：钩子配置路径
- 未识别字段保留不报错

### registry.json（全局注册表）

```json
{
  "plugins": {
    "superpowers": {
      "publisher": "claude-plugins-official",
      "version": "4.3.1",
      "installed_at": "2026-05-20T14:00:00Z",
      "source": "local:/path/to/superpowers"
    }
  }
}
```

### 安装来源

```rust
enum PluginSource {
    Local { path: PathBuf },  // 从本地目录安装
    Git { url: String },      // 从 Git 仓库安装（后续）
}
```

MVP 阶段只支持 Local，Git 后续迭代。

### 核心操作

```rust
// brain-plugin/src/plugin_manager.rs

struct PluginManager {
    registry: PluginRegistry,
    plugins_dir: PathBuf,  // ~/.ai-brain/plugins/
}

impl PluginManager {
    fn load(plugins_dir: &Path) -> Result<Self>;
    fn install(&mut self, source: &Path) -> Result<String>;
    fn uninstall(&mut self, name: &str) -> Result<()>;
    fn list(&self) -> Vec<&PluginMeta>;
    fn plugin_dir(&self, name: &str) -> Option<PathBuf>;
    fn skill_roots(&self) -> Vec<PathBuf>;       // 所有插件的 skills/ 路径
    fn mcp_configs(&self) -> Vec<PathBuf>;       // 所有插件的 MCP 配置路径
}
```

### 安装流程

```
plugin install /path/to/superpowers
  1. 读取源目录的 plugin.json，校验必填字段
  2. 生成目标路径: ~/.ai-brain/plugins/cache/{publisher}/{name}/{version}/
  3. 复制文件到目标路径
  4. 更新 registry.json
  5. 返回安装信息
```

### 卸载流程

```
plugin uninstall superpowers
  1. 从 registry.json 读取插件信息
  2. 删除 ~/.ai-brain/plugins/cache/{publisher}/{name}/
  3. 从 registry.json 移除条目
  4. 保存 registry.json
```

## 6. McpClientPool 层

### MCP 服务器配置格式

来源：插件内的 `mcpServers` + 用户配置 `~/.ai-brain/mcp/mcp-servers.json`。

```json
{
  "mcpServers": {
    "context7": {
      "command": "npx",
      "args": ["-y", "@upstreamapi/context7-mcp@latest"],
      "env": { "API_KEY": "${CONTEXT7_API_KEY}" }
    },
    "memory-system": {
      "type": "sse",
      "url": "http://localhost:8080/sse"
    }
  }
}
```

传输协议支持：
- `command`（stdio）：子进程 stdin/stdout JSON-RPC
- `type: "sse"`：Server-Sent Events
- `type: "http"`：Streamable HTTP

### 连接生命周期

```
启动阶段
  1. 读取所有 MCP 配置（插件 + 用户）
  2. 对每个服务器：
     a. rmcp Client::connect(transport) 建立连接
     b. client.list_tools() 获取工具列表
     c. 注册到工具池: mcp__{server}__{tool} 格式
     d. 连接失败 → 记录警告，不阻塞启动
  3. 启动健康检查循环（每 60s ping）

运行阶段
  主脑调用 mcp__{server}__{tool}
    → McpClientPool 路由到对应 client
    → client.call_tool(name, arguments)
    → 返回结果

关闭阶段
  逐一 client.shutdown()
  等待子进程退出（stdio 模式）
```

### 关键数据结构

```rust
// brain-mcp/src/client_pool.rs

use rmcp::Client;

struct McpServerEntry {
    name: String,
    client: Client,
    tools: Vec<ToolDescriptor>,
    transport: TransportType,
    status: ServerStatus,
}

enum ServerStatus {
    Connected,
    Disconnected,
    Error(String),
}

struct McpClientPool {
    servers: HashMap<String, McpServerEntry>,
}

impl McpClientPool {
    async fn connect_all(configs: &McpConfigs) -> Result<Self>;
    async fn reconnect(&mut self, server: &str) -> Result<()>;
    fn list_tools(&self) -> Vec<ToolDescriptor>;
    async fn execute(&self, tool_call: &ToolCall) -> Result<ToolExecutionResult>;
    async fn shutdown(&mut self) -> Result<()>;
}
```

### 工具名映射

沿用 `runtime/mcp.rs` 已有命名规范：
```
"context7" 服务器的 "query-docs" 工具
  → 注册为 "mcp__context7__query_docs"
  → 主脑 LLM 看到这个工具名
  → 调用时 McpClientPool 解析出 server + tool
  → 转发给 rmcp client
```

## 7. 与主脑集成

### 编排器启动流程

```rust
async fn init_brain_system(config: &Config) -> BrainSystem {
    // 1. PluginManager
    let plugin_mgr = PluginManager::load("~/.ai-brain/plugins/")?;

    // 2. SkillLoader
    let skill_roots = vec![
        "{cwd}/.ai-brain/skills/",
        plugin_mgr.skill_roots(),
        "~/.ai-brain/skills/",
    ];
    let skill_catalog = SkillCatalog::scan_all(&skill_roots)?;

    // 3. McpClientPool
    let mcp_configs = collect_mcp_configs(
        plugin_mgr.mcp_configs(),
        "~/.ai-brain/mcp/mcp-servers.json",
    );
    let mcp_pool = McpClientPool::connect_all(&mcp_configs).await?;

    // 4. 构建主脑
    let tool_executor = RealToolExecutor::new(
        skill_catalog, mcp_pool, memory_brain,
    );
    let mut main_brain = MainBrain::new(llm, tool_executor, config);

    // 5. 注册工具列表
    let mut all_tools = mvp_tool_definitions();
    all_tools.push(skill_catalog.tool_definition());
    all_tools.extend(mcp_pool.list_tool_definitions());
    main_brain.register_tools(all_tools);

    BrainSystem { main_brain, skill_catalog, mcp_pool, plugin_mgr }
}
```

### RealToolExecutor 路由

```rust
match name.as_str() {
    "search_memory" | "list_recent_memories" => { /* 记忆脑，不变 */ }
    "Skill" => { /* SkillCatalog resolve + load_content */ }
    n if n.starts_with("mcp__") => { /* McpClientPool 路由 */ }
    _ => { /* 内建工具，不变 */ }
}
```

### system prompt 注入

```rust
fn build_system_prompt(&self, skill_catalog: &SkillCatalog) -> String {
    let mut prompt = base_system_prompt();
    let skills_xml = skill_catalog.summary_for_prompt();
    if !skills_xml.is_empty() {
        prompt.push_str("\n\n## 可用技能\n");
        prompt.push_str("调用格式：Skill({ skill: \"技能名\" }) ");
        prompt.push_str("或 Skill({ skill: \"插件名:技能名\" })\n");
        prompt.push_str(&skills_xml);
    }
    prompt
}
```

### 评估脑复用

```rust
let skill_catalog = Arc::new(SkillCatalog::scan_all(&skill_roots)?);
main_brain.set_skill_catalog(skill_catalog.clone());
eval_brain.set_skill_catalog(skill_catalog);
```

评估脑通过同一个 `summary_for_prompt()` 获取技能目录，Skill 调用也走同一个 resolve 逻辑。

## 8. CLI 命令

```
/plugin install /path/to/plugin     # 安装插件
/plugin uninstall <name>            # 卸载插件
/plugin list                        # 列出已安装插件
/mcp status                         # 查看 MCP 服务器连接状态
/mcp restart <server>               # 重连某个 MCP 服务器
```

## 9. 清理项

- 删除 `tools/src/lib.rs` 中的 `resolve_skill_path()` + `parse_skill_description()`
- 删除 `commands/src/lib.rs` 中的 `discover_skill_roots()` + `parse_skill_frontmatter()`
- 删除 `brain-eval/src/skills/types.rs` 中的 `SkillRegistry`
- 删除 `runtime/src/mcp.rs` 中的 stub MCP 工具（由 brain-mcp 接管）
