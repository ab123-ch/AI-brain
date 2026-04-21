# 评估脑设计 — EvalBrain

> 常驻后台、静默监听、条件触发。基于记忆脑的踩坑库和用户画像做质量审核。

## 1. 定位

评估脑跟主脑一起启动，常驻运行。主脑每次产生最终输出后自动触发评估。

**不与用户直接交互。通过插入反馈消息影响主脑行为。**

## 2. 核心设计原则

- **基于记忆脑的数据做评估**，不是凭空判断
- **踩过的坑不能再踩**
- **用户纠正过的不重复犯**
- 发现问题 → 反馈插入主脑对话历史，主脑自己决定如何修正

## 3. 核心数据结构

```rust
struct EvalBrain {
    llm: Arc<dyn LlmProvider>,

    // 记忆脑引用（只读，获取评估依据）
    memory_brain: Arc<Mutex<MemoryBrain>>,

    // 进度
    progress_tx: Option<mpsc::Sender<ProgressEvent>>,

    config: EvalConfig,
}

struct EvalConfig {
    enabled: bool,           // 是否启用评估
    auto_correct: bool,      // 自动纠正（不需要用户确认）
}

struct EvalResult {
    passed: bool,
    issues: Vec<EvalIssue>,
}

struct EvalIssue {
    category: EvalIssueCategory,
    severity: Severity,          // Warning / Critical
    description: String,        // "又写了 TODO"
    pitfall_ref: Option<String>, // 关联的踩坑记录 ID
    suggestion: String,         // "请完成实际逻辑实现"
}

enum EvalIssueCategory {
    RepeatedPitfall,      // 重复踩坑
    ViolatedPreference,   // 违反用户偏好
    Laziness,             // 偷懒行为
    FactualError,         // 事实性错误
    IgnoredInstruction,   // 忽略用户指令
}

enum Severity {
    Warning,    // 提醒但不拦截
    Critical,   // 必须修正
}
```

## 4. 评估流程

### 触发时机
主脑每次产生最终文本输出后，自动触发评估。

### 执行过程

```
主脑输出最终文本
  │
  ├─ 评估脑接收输出 + 当前用户输入
  │
  ├─ 从记忆脑获取评估依据:
  │   ├─ 用户画像 (UserProfile)
  │   ├─ 踩坑库 (PitfallDatabase)
  │   └─ 自进化规则 (EvolutionRules)
  │
  ├─ 独立 LLM 调用，专用 system prompt:
  │   输入: 主脑输出 + 用户输入 + 评估依据
  │   输出: EvalResult (passed + issues)
  │
  ├─ 发送 ProgressEvent::EvaluationResult
  │
  └─ 处理评估结果:
      ├─ passed=true → 输出给用户
      └─ passed=false → 构造反馈消息，插入主脑对话历史
```

### 4.1 反馈插入机制

```
主脑 messages:
  [user: "帮我写个素数判断函数"]
  [assistant: "fn is_prime(n: u64) -> bool {\n    // TODO\n}"]
  [eval_feedback: "⚠️ 你写了 TODO 而不是实际实现（踩坑#3: 用户明确要求不能写TODO）。
                    请完成素数判断的实际逻辑。"]         ← 评估脑插入
  [assistant: "抱歉，补完：\nfn is_prime(n: u64) -> bool {\n    if n <= 1..."]
```

反馈消息使用 `MessageRole::User`（让 LLM 认为是用户的补充要求），
或者自定义一个特殊 role 标记为评估反馈。

## 5. 评估 Prompt

```markdown
你是一个严格的质量审计员。你的任务是检查 AI 助手的输出是否存在问题。

## 评估依据

### 用户画像
{user_profile}

### 已知踩坑点（这些错误不能再犯）
{pitfalls}

### 自进化规则（必须遵守）
{evolution_rules}

## 待评估内容

用户输入: {user_input}
AI 输出: {ai_output}

## 评估检查清单

逐项检查以下问题：

1. **重复踩坑**: 输出是否犯了已知踩坑点中的错误？
   - 逐个对比踩坑库中的每条记录
   - 如果输出与踩坑模式匹配 → 标记为 Critical

2. **违反用户偏好**: 输出是否违反了用户的明确偏好？
   - 语言偏好（中文/英文）
   - 输出格式偏好
   - 技术栈偏好

3. **偷懒行为**: 输出中是否有以下偷懒行为？
   - 写了 TODO/FIXME 而不是实际实现
   - 跳过了复杂的逻辑
   - 只给了思路没给代码（当用户要求代码时）
   - 用 "..." 省略了应该写完的内容

4. **事实性错误**: 输出中是否有明显的事实错误？

5. **忽略用户指令**: 是否忽略了用户的明确要求？

## 输出格式

返回 JSON:
{
  "passed": true/false,
  "issues": [
    {
      "category": "Laziness",
      "severity": "Critical",
      "description": "写了 TODO 而不是实际实现",
      "pitfall_ref": "pitfall_003",
      "suggestion": "请完成 is_prime 函数的实际素数判断逻辑"
    }
  ]
}

如果没有发现问题，返回 {"passed": true, "issues": []}
```

## 6. 评估脑不做的事

- ❌ 不评估"是否完整回答了用户的问题"（这是主观的）
- ❌ 不评估输出的创意或质量（这是主观的）
- ❌ 不做前置检查（不在主脑工作前介入）
- ❌ 不参与对话流程管理

评估脑**只关注已知错误的重复**和**用户偏好的违反**。
它是"错题本检查员"，不是"作文评分老师"。

## 7. 与记忆脑的协作

```
记忆脑产出:
  ├─ 踩坑库 → 评估脑用来检查是否重复踩坑
  ├─ 用户画像 → 评估脑用来检查是否违反偏好
  └─ 自进化规则 → 评估脑用来检查是否违反规则

评估脑产出:
  └─ 新的拦截记录 → 反馈给记忆脑，可能产生新的踩坑记录
```

如果评估脑拦截了一个新的问题类型（踩坑库里没有的），
记忆脑在下一轮四步分析时会将其收录为新的踩坑记录。
这样**评估脑和记忆脑形成闭环**：

```
踩坑 → 记忆脑记录 → 评估脑检查 → 再踩？→ 评估脑拦截 → 记忆脑确认
                                                    ↑
                                               形成闭环
```
