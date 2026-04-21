# 记忆脑设计 — MemoryBrain

> 常驻后台、静默监听、条件触发。负责四步分析、上下文管理、自进化。

## 1. 定位

记忆脑跟主脑一起启动，常驻运行。大部分时间静默监听主脑的所有交互，
条件满足时触发四步分析。

**不与用户直接交互。不中断主脑工作。**

## 2. 核心数据结构

### 2.1 存储分层（复用现有设计）

| 层 | 内容 | 格式 | 示例 |
|----|------|------|------|
| L1 Raw | 全量原始数据 | JSONL | 完整的对话记录、工具调用和结果 |
| L2 Index | 索引摘要 | JSON | 关键词索引 + 映射到 L1 的 SourceRef |
| L3 Experience | 经验包 | JSON | trigger_pattern + context_snippet + 避坑规则 |

### 2.2 记忆脑状态

```rust
struct MemoryBrain {
    llm: Arc<dyn LlmProvider>,

    // 存储
    raw_layer: RawLayer,            // L1 全量
    index_layer: IndexLayer,        // L2 索引
    experience_layer: ExperienceLayer, // L3 经验包

    // 新增：分析结果
    user_profile: UserProfile,      // 用户画像
    pitfall_db: PitfallDatabase,    // 踩坑库
    evolution_rules: Vec<EvolutionRule>, // 自进化规则

    // 上下文状态
    current_summary: Option<BrainState>, // 当前最新的 brain_state
    rounds_since_summary: usize,         // 距上次总结的轮次

    // 触发配置
    config: MemoryConfig,
}

struct MemoryConfig {
    summary_interval: usize,       // 每 N 轮触发总结（默认 10）
    context_threshold: f64,        // token 使用率阈值（默认 0.8）
    preserve_recent_turns: usize,  // 重建时保留最近 N 轮（默认 4）
}
```

## 3. 四步分析流程（顺序执行）

### 触发条件
- 主脑每处理一轮用户输入 → rounds_since_summary += 1
- rounds_since_summary >= summary_interval → 触发
- 或主脑 token 估算 > context_threshold → 强制触发

### 输入
- 最近 N 轮的完整对话历史（从 L1 获取）
- 已有的用户画像
- 已有的踩坑库
- 已有的自进化规则

### 执行过程

```
四步分析 — 独立 LLM 调用，专用 system prompt

输入: 最近 10 轮对话 + 已有记忆数据
  │
  ▼
第一步：事实总结
  │  "用户说了什么，我做了什么，调了什么工具，结果如何"
  │  输出: 事实列表 + 映射索引（原始内容位置）
  │
  ▼
第二步：用户画像分析（基于事实总结）
  │  "用户偏好什么、讨厌什么、情绪如何、习惯是什么"
  │  输出: 画像更新增量（追加到已有画像）
  │
  ▼
第三步：踩坑点分析（基于事实总结）
  │  "什么工具调用失败了、为什么、怎么避免"
  │  "用户纠正了什么、正确做法是什么"
  │  输出: 新增踩坑记录
  │
  ▼
第四步：自进化规则（基于前三步）
  │  "从这些经历中提取的通用规则"
  │  输出: 新增/更新的自进化规则
  │
  ▼
结果:
  ├─ 更新 current_summary（brain_state）
  ├─ 更新 user_profile
  ├─ 更新 pitfall_db
  ├─ 更新 evolution_rules
  └─ 存储到 L2/L3 层
```

### 四步分析的 LLM Prompt 模板

```markdown
你是一个记忆分析引擎。请分析以下对话记录，按四个步骤输出。

## 已有的记忆上下文
- 用户画像: {user_profile}
- 已知踩坑点: {pitfalls}
- 自进化规则: {evolution_rules}

## 最近 {N} 轮对话
{conversation_history}

## 输出格式

### 第一步：事实总结
对每一轮对话，用一句话总结：
- 格式: "用户[意图]，我调用了[工具](参数摘要)，结果[结果摘要]"
- 附带映射索引: 原始数据在 L1 的存储位置

### 第二步：用户画像更新
基于事实总结，识别：
- 显性偏好: 用户明确说过的要求
- 隐性偏好: 从行为中推断的偏好
- 情绪状态: 用户的情绪变化
- 习惯模式: 用户的使用习惯

### 第三步：踩坑点
识别以下类型的问题：
- 工具调用失败: 什么工具、什么参数、为什么失败、正确做法
- 答案错误: 用户纠正了什么、正确答案是什么
- 格式问题: 输出格式不符合用户要求
- 偷懒行为: 跳过了复杂逻辑、写了 TODO 等

### 第四步：自进化规则
从以上分析中提炼可复用的规则：
- 通用规则: 适用于所有场景的避坑指南
- 工具规则: 特定工具的使用注意事项
- 用户规则: 特定用户的个人化规则
```

## 4. 上下文管理

### 4.1 监听主脑

```
主脑每次完成一轮:
  │
  ├─ 完整交互落盘到 L1 Raw
  │   { role, blocks, timestamp, tool_name, tool_input, tool_output, duration_ms }
  │
  └─ 通知记忆脑: rounds_since_summary += 1
```

### 4.2 Token 估算与触发

```
记忆脑监听主脑的 token 估算:
  │
  ├─ < 60%  → 什么都不做
  ├─ 60-80% → 如果 rounds_since_summary >= interval，触发四步分析
  └─ > 80%  → 强制触发四步分析 + 上下文重建
```

### 4.3 上下文重建

```
记忆脑执行上下文重建:
  │
  ├─ 1. 取当前 current_summary（最新 brain_state）
  │     如果没有 → 先执行一次四步分析
  │
  ├─ 2. 清空主脑的 messages
  │
  ├─ 3. 注入重建内容:
  │     [SYSTEM_PROMPT]
  │     [TOOL_DEFINITIONS]
  │     [user: "## 之前的工作上下文\n{brain_state}"]
  │     [最近 N 轮对话保留]
  │
  └─ 4. 主脑继续工作
```

### 4.4 brain_state 内容格式

```markdown
## 项目上下文
- 用户在开发 AI Brain Rust 项目
- 工作目录: /Users/chenh/RustObject/claw-code-parity
- 使用 glm-5.1 主力模型

## 用户偏好
- 中文交流
- 不用 TODO，必须写实际逻辑
- 宁波用户，天气查询默认宁波

## 已完成的工作
- 重构为三脑架构
- 实现了工具调用参数展示
- [映射索引: L1#20260417_001-L1#20260417_015]

## 踩坑记录
- #1: WebFetch 超时 → 优先用 WebSearch
- #2: 回答北京天气 → 用户在宁波，天气查询需确认位置
- #3: 写了 TODO → 用户要求实际实现

## 自进化规则
- 天气查询先确认用户位置
- read_file 大文件时截断或只读关键部分
- 所有代码输出必须包含实际逻辑，不能写 TODO
```

## 5. 用户画像

```rust
struct UserProfile {
    // 显性偏好（用户明确说过）
    preferences: Vec<String>,       // "用 Rust"、"回答简洁"、"中文"

    // 隐性偏好（从行为推断）
    inferred_habits: Vec<String>,   // "宁波用户"、"工作时段活跃"

    // 情绪记录
    emotion_history: Vec<EmotionRecord>, // 最近 N 次的情绪状态

    // 位置
    location: Option<String>,       // "宁波"

    // 技术栈
    tech_stack: Vec<String>,        // ["Rust", "Spring Boot", "Kotlin"]

    // 沟通风格
    communication_style: String,    // "简洁直接，不耐烦长篇解释"
}
```

## 6. 踩坑库

```rust
struct PitfallRecord {
    id: String,
    category: PitfallCategory,
    description: String,          // "回答了北京天气"
    root_cause: String,           // "没有确认用户位置"
    user_correction: String,      // "用户纠正为宁波"
    correct_approach: String,     // "天气查询先确认用户位置"
    occurrence_count: usize,      // 出现次数
    last_occurrence: DateTime<Utc>,
    source_ref: SourceRef,        // 映射到 L1 原始数据
}

enum PitfallCategory {
    ToolCallFailure,    // 工具调用失败
    WrongAnswer,        // 答案错误
    FormatIssue,        // 格式问题
    Laziness,           // 偷懒行为
    UserCorrection,     // 用户纠正
    MissedContext,      // 丢失上下文
}
```

## 7. 自进化规则

```rust
struct EvolutionRule {
    id: String,
    rule: String,                 // "天气查询先确认用户位置"
    source_pitfalls: Vec<String>, // 来源踩坑记录 ID
    confidence: f64,              // 置信度（被验证次数越多越高）
    applicable_contexts: Vec<String>, // 适用场景
}
```

## 8. 召回机制（评估脑和主脑使用）

```
评估脑/主脑需要记忆数据时:
  │
  ├─ 关键词搜索 L2 Index → 获取相关条目
  ├─ 获取用户画像
  ├─ 获取踩坑库
  ├─ 获取自进化规则
  │
  └─ 需要完整原始数据时 → 通过 SourceRef 从 L1 获取
```
