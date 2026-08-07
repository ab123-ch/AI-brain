# 实例 LLM 模型目录设计

## 目标

让 Web 协作房间中的每个智脑实例都能从 `~/.ai-brain/config.toml` 声明的模型目录中选择模型；首批提供 DeepSeek V4 Pro、DeepSeek V4 Flash、Gemini 2.5 Pro 和 Gemini 2.5 Flash。

## 背景

当前 Web 只允许 `main` 一种模型策略。它解析为 `deepseek-v4-pro`，因此即使配置文件中已有 Gemini provider，实例编辑器仍只显示 DeepSeek。

同时，现有 Gemini provider 使用 Google 原生 API。用户确认改用 OpenAI 兼容中转：`https://ai.xfws88.com`。该中转提供 Gemini 和 DeepSeek 模型，且沿用当前 Gemini 配置中的密钥。

## 决策

采用配置驱动的静态模型目录，不在运行时请求模型广场。

- 模型可见性完全由本地 `config.toml` 决定；不会暴露未授权或临时下线的远端模型。
- 每条目录项包含稳定策略 ID、显示名称、provider 名称和模型 ID。
- 实例持久化目录项的策略 ID，而不是直接保存 provider/model 字符串。
- Gemini provider 改为 OpenAI 兼容协议和中转地址；不使用 Gemini 原生请求格式。
- 现有 `main` 策略保留，已有房间与成员无需迁移；新目录项可与它并存。

## 配置格式

在 `[llm]` 下新增 `instance_models` 数组：

```toml
[[llm.instance_models]]
id = "deepseek-v4-pro"
label = "DeepSeek V4 Pro"
provider = "deepseek"
model = "deepseek-ai/deepseek-v4-pro"

[[llm.instance_models]]
id = "gemini-2-5-pro"
label = "Gemini 2.5 Pro"
provider = "gemini"
model = "gemini-2.5-pro"
```

Gemini provider 的配置改为：

```toml
[llm.providers.gemini]
api_base = "https://ai.xfws88.com/v1"
api_key_env = "GEMINI_API_KEY"
kind = "openai"
```

保留已存在的 `api_key` 配置值；实现不读取、打印或迁移密钥。

## 运行时数据流

```text
config.toml instance_models
  -> LlmConfig 解析和校验
  -> CollaborationRuntime 生成可选策略与展示详情
  -> RoomSnapshot.model_policies / model_policy_details
  -> Web 成员编辑器下拉框
  -> 成员持久化 model_policy
  -> Orchestrator 按策略创建 OpenAI 兼容客户端
```

目录项必须引用已存在的 provider，并且 provider 必须能解析 API key；无效目录项在启动时记录可理解的错误并且不进入 Web 下拉框。创建或修改成员时，服务端仍按 allowlist 校验策略 ID。

## Web 行为

- 新建或编辑实例时，模型下拉框显示配置目录中的 `label`、provider 和模型 ID。
- 已存在的 `main` 成员继续正常显示并可运行。
- 配置文件修改在重启智脑后生效；本次不引入 Web 内修改密钥或模型目录的功能。

## 测试

- 为 `LlmConfig` 增加目录解析、未知 provider 拒绝和策略到 provider/model 的解析测试。
- 为协作运行时增加“目录项进入 Web 快照”的测试。
- 为协作仓库增加目录策略可用于创建/配置成员的测试。
- 为前端选择器增加显示配置标签的测试。
- 运行格式检查、受影响 Rust crate 测试、前端 Node 测试和 release 构建。

## 非目标

- 不自动从模型广场同步目录。
- 不通过 Web 编辑 API Key、代理或配置文件。
- 不添加 GLM、图像、语音或 embedding 模型；它们可在后续通过同一目录格式加入。
