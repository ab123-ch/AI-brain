# 实例 LLM 模型目录实施计划

> **给实施者：** 必须使用 `executing-plans` 技能逐项执行本计划。

**目标：** 让每个 Web 协作实例从本地 `config.toml` 的模型目录选择可用 LLM，并让 Gemini 通过用户指定的 OpenAI 兼容中转调用。

**架构：** `brain-llm` 新增配置驱动的实例模型定义和统一策略解析/客户端创建入口。Web 启动阶段用可执行的策略目录扩展协作 allowlist，运行时把策略详情放入房间快照；Orchestrator 用相同策略创建对应客户端。前端只渲染服务端下发的目录，不持有密钥或端点信息。

**技术栈：** Rust、Serde/TOML、Axum、SQLite、原生浏览器 JavaScript、Node 内置测试运行器。

---

### 任务 1：在 LLM 配置层声明并解析实例模型目录

**文件：**
- 修改：`rust/crates/brain-llm/src/config.rs`
- 修改：`rust/crates/brain-llm/src/lib.rs`（如当前公共 re-export 不足）
- 测试：`rust/crates/brain-llm/src/config.rs` 中现有 `tests` 模块

**步骤：**

1. 在 `LlmSection` 增加 `#[serde(default)] pub instance_models: Vec<InstanceModelConfig>`，并定义可序列化/反序列化的 `InstanceModelConfig { id, label, provider, model }`。为 `ResolvedModelPolicy` 增加 `label` 字段，以便 API 快照向前端传递配置中的人类可读名称；遗留策略的 label 应回退为 `policy_id`。
2. 在 `LlmConfig` 新增策略解析入口，明确区分旧的副脑别名和实例目录项：

   ```rust
   pub fn resolve_instance_model_policy(&self, id: &str) -> Result<ResolvedModelPolicy>;
   pub fn available_instance_model_policies(&self) -> Vec<ResolvedModelPolicy>;
   pub fn create_model_policy_client(&self, policy_id: &str) -> Result<Box<dyn LlmProvider>>;
   ```

   `available_instance_model_policies` 必须保留可运行的 `main`，然后按 TOML 顺序加入目录项；目录项的 provider 不存在、ID 重复/为空、或 API key 无法解析时，跳过该项并记录中文 `tracing::warn!`，绝不能让它进入 Web allowlist。不要输出 key、完整配置内容或环境变量值。
3. 将 `create_brain_client` 内的 provider-kind/client 构造代码提取为接收 `ResolvedModelPolicy` 的私有/公共共享方法。这样 `main` 和 `instance_models` 都会按 provider 的 `kind`、端点、代理和 token 参数创建相同的 OpenAI 兼容或 Gemini 原生客户端，避免两条实现分叉。
4. 保持 `model_for_brain`、`provider_for_brain`、`params_for_brain` 的遗留语义不变；`create_model_policy_client` 先尝试实例目录，未命中再保留对旧 brain policy 的兼容，从而让已有成员的 `main` 可继续运行。
5. 在默认配置构造函数初始化空 `instance_models`，并更新所有直接构造 `LlmSection` 的测试 fixture。
6. 在同模块先写失败测试，再实现：
   - TOML 中的两个有效目录项按声明顺序得到正确 `policy_id`、`label`、provider、model 和默认参数；
   - 未知 provider、空/重复 ID、无可用 key 的目录项不会出现在 `available_instance_model_policies`；
   - 目录项以 `kind = "openai"` 走 `OpenAiCompatClient`，且 `create_model_policy_client` 的 `client.model()` 为目录模型 ID；
   - `main` 的旧解析和客户端创建测试仍通过。

**验证：**

```powershell
cd rust
cargo test -p brain-llm config::tests
cargo fmt -- --check
```

### 任务 2：让 Web 协作 allowlist 与目录策略使用同一份启动快照

**文件：**
- 修改：`rust/crates/ai-brain-cli/src/api_server.rs`
- 修改：`rust/crates/ai-brain-cli/src/web/collaboration.rs`
- 修改：`rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs`
- 测试：`rust/crates/ai-brain-cli/src/web/collaboration.rs`、`rust/crates/ai-brain-cli/src/web/collaboration_runtime.rs` 中现有测试模块

**步骤：**

1. 在 Web 启动处加载一次 `LlmConfig`，调用 `available_instance_model_policies`，并将结果同时交给协作配置和运行时。若完整 LLM 配置无法解析，记录原因并以仅含遗留 `main` 的安全回退启动；不能因单个目录项失效导致 Web 服务整体退出。
2. 给 `CollaborationConfig` 增加只在启动期调用的构造/覆盖方法，例如 `with_available_model_policies(&[ResolvedModelPolicy])`。它应以解析后的策略 ID 覆盖 `allowed_model_policies`，并确保 `default_model_policy` 在名单里；若配置的默认值不可用，回退到第一个可用策略。不要把未校验的原始 TOML ID 写入数据库。
3. 将 `CollaborationRuntime::start` 改为接收已解析的 `Vec<ResolvedModelPolicy>`，不再在内部第二次读配置并调用 `resolve_model_policy_details`。这样服务端校验、成员模板持久化、下拉选项和任务分派都来自同一快照。移除或收缩旧 helper，保留必要的 `main` 回归测试。
4. 继续使用 `CollaborationRepository::new` 的现有模板 upsert。启动时把更新后的 allowlist 写回 SQLite 的成员模板，使新建和编辑成员立即接受新增策略；不迁移/改写已存储成员的 `model_policy`。
5. 编写失败优先的测试：
   - 含 `main` 和两个目录策略的配置会让 `RoomSnapshot.model_policies` 和 `model_policy_details` 依次出现三项，目录项标签不丢失；
   - 协作仓库能用目录策略创建和更新成员；不在目录/allowlist 内的策略仍被拒绝；
   - 仅 `main` 的老配置保持原有快照和默认成员行为。

**验证：**

```powershell
cd rust
cargo test -p ai-brain-cli web::collaboration
cargo test -p ai-brain-cli web::collaboration_runtime
```

### 任务 3：使执行器按成员选择的实例模型真正发起调用

**文件：**
- 修改：`rust/crates/ai-brain-cli/src/orchestrator.rs`
- 测试：`rust/crates/ai-brain-cli/src/orchestrator.rs` 中相邻的成员查询/执行测试

**步骤：**

1. 定位协作成员执行分支（当前调用 `create_brain_client(&model_policy)` 的位置），将“策略是否存在”的判断和客户端构造统一替换为 `LlmConfig::create_model_policy_client(&model_policy)`。
2. 保留现有的 `MemberQueryError::before_execution` 包装，但把“未知策略”“provider 不可用”“缺少 key”等错误保留为可诊断中文原因；不可默默降级为 default DeepSeek 或 `main`。
3. 用纯配置/客户端层级测试或已有 Orchestrator 单元测试验证：目录 ID 会得到声明的 provider/model，`main` 仍走旧别名，未知 ID 在执行前失败。测试不能调用外网或真实 key。

**验证：**

```powershell
cd rust
cargo test -p ai-brain-cli orchestrator
```

### 任务 4：在模型下拉框和实例卡片显示配置标签

**文件：**
- 新增：`rust/crates/ai-brain-cli/src/web/static/model_catalog.js`
- 新增：`rust/crates/ai-brain-cli/src/web/static/model_catalog.test.js`
- 修改：`rust/crates/ai-brain-cli/src/web/static/index.html`
- 修改：`rust/crates/ai-brain-cli/src/web/static/app.js`
- 修改：`rust/crates/ai-brain-cli/src/api_server.rs`（若静态文件路由需显式 `include_str!` 注册）

**步骤：**

1. 新建无 DOM 依赖的浏览器全局工具 `window.ModelCatalog`，提供：

   ```javascript
   function optionText(policy) {
       return `${policy.label || policy.model} · ${policy.provider} · ${policy.model}`;
   }
   function compactLabel(policy) {
       return policy.label || policy.model || policy.policy_id;
   }
   ```

   用 UMD/CommonJS 兼容导出，使 Node 测试无需浏览器环境。
2. 在 `index.html` 中先加载该工具，再加载 `app.js`；在静态路由中以与现有 `mentions.js` 相同的缓存和 `Content-Type` 方式提供它。
3. 让 `fillModelSelect` 以 `optionText(detail)` 作为目录模型的选项文字和 title。成员卡片显示 `compactLabel(detail)`，title 显示 label、provider、模型 ID 与策略 ID。缺少详情的旧数据继续回退到 `member.model_policy`。
4. 添加 `model_catalog.test.js`，使用 `node --test` 覆盖标签优先、缺少标签回退、provider/model 拼接及 legacy policy 回退；不把密钥或端点送到浏览器。

**验证：**

```powershell
cd rust\crates\ai-brain-cli\src\web\static
node --test model_catalog.test.js mentions.test.js
```

### 任务 5：更新默认模板和当前实例配置为用户指定的目录及 Gemini 中转

**文件：**
- 修改：`rust/crates/ai-brain-cli/src/init.rs`
- 修改（用户本机运行配置，不提交）：`C:\Users\16038\.ai-brain\config.toml`
- 测试：任务 1、2 的配置解析测试

**步骤：**

1. 在 `CONFIG_TEMPLATE` 中说明 `[[llm.instance_models]]` 的用途，并提供不含任何密钥的注释示例。保留现有 provider 与副脑配置兼容性；示例的 Gemini 中转用 `api_base = "https://ai.xfws88.com/v1"` 与 `kind = "openai"`。
2. 只编辑本机 Gemini provider 的非敏感字段：端点替换为 `https://ai.xfws88.com/v1`，`kind` 替换为 `openai`。保留现有 `api_key_env` 与 `api_key` 的值，绝不读取、输出、提交或改写 key。
3. 在当前配置加入以下首批实例目录。DeepSeek Pro 保持目前已在用的模型 ID；Flash 使用同一 provider 的 V4 Flash ID。Gemini 项通过已转换的 Gemini 中转 provider：

   ```toml
   [[llm.instance_models]]
   id = "deepseek-v4-pro"
   label = "DeepSeek V4 Pro"
   provider = "deepseek"
   model = "deepseek-v4-pro"

   [[llm.instance_models]]
   id = "deepseek-v4-flash"
   label = "DeepSeek V4 Flash"
   provider = "deepseek"
   model = "deepseek-v4-flash"

   [[llm.instance_models]]
   id = "gemini-2-5-pro"
   label = "Gemini 2.5 Pro"
   provider = "gemini"
   model = "gemini-2.5-pro"

   [[llm.instance_models]]
   id = "gemini-2-5-flash"
   label = "Gemini 2.5 Flash"
   provider = "gemini"
   model = "gemini-2.5-flash"
   ```

4. 使用 TOML 解析或 CLI 加载校验配置，不执行真实模型请求。将本机配置视为机密运行态：仅报告是否通过以及可见策略数量。

**验证：**

```powershell
cd rust
cargo test -p brain-llm config::tests
```

### 任务 6：全量验证、构建、重启并手工验收

**文件：**
- 不新增代码文件；检查本任务涉及的所有文件

**步骤：**

1. 运行格式化与受影响 crate 的 lint/test：

   ```powershell
   cd rust
   cargo fmt -- --check
   cargo clippy -p brain-llm -p ai-brain-cli --all-targets -- -D warnings
   cargo test -p brain-llm
   cargo test -p ai-brain-cli
   ```

2. 若工作区基线测试在 Windows 上失败，先在未改动路径重现并记录范围；不得把基线失败归因于本次修改。继续运行本次新增/受影响的定向测试，并报告清楚。
3. 构建 release：

   ```powershell
   cargo build --release -p ai-brain-cli --bin ai-brain
   ```

4. 确认当前 Web 进程的精确 PID 后停止它，启动新 release 的 `ai-brain web --addr 127.0.0.1:8080`。用 `Invoke-WebRequest http://127.0.0.1:8080/` 验证 HTTP 200。
5. 在浏览器中创建或编辑一个实例，确认下拉框含 `main`、DeepSeek V4 Pro/Flash、Gemini 2.5 Pro/Flash；保存并读取房间快照，确认成员保留所选策略 ID。不要为了验收而发送真实模型请求或暴露密钥。
6. 使用 `git diff --check` 和 `git status --short` 审查改动；只提交仓库代码、测试、模板和计划文档，不提交 `C:\Users\16038\.ai-brain\config.toml` 或用户现有未跟踪文件。建议提交信息：`feat(web): add instance LLM catalog`。
