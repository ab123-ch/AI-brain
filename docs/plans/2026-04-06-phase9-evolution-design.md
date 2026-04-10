# Phase 9: 进化机制设计

> 日期：2026-04-06
> 状态：已批准

---

## 核心决策

| 决策 | 选择 |
|------|------|
| 创建触发 | 用户手动 + LLM 建议式自动 |
| 休眠策略 | 硬休眠（Drop 对象，配置持久化到磁盘，唤醒时重新初始化） |
| 架构方案 | 新增 `brain-evolution` crate，BrainRegistry 注册中心 |

---

## Crate 结构

```
rust/crates/brain-evolution/
├── Cargo.toml
└── src/
    ├── lib.rs              # 公共导出
    ├── registry.rs         # BrainRegistry 注册中心
    ├── template.rs         # 副脑模板 (BrainTemplate)
    ├── dormancy.rs         # 硬休眠管理 (DormancyManager)
    └── suggestion.rs       # LLM 建议式创建 (SuggestionEngine)
```

---

## 核心类型

### 副脑模板 (template.rs)

```rust
pub struct BrainTemplate {
    pub name: String,
    pub description: String,
    pub prompt_template: String,
    pub fast_think_rules: Vec<String>,
    pub initial_weight: f64,
    pub parent_brain: Option<BrainId>,
    pub capabilities: Vec<String>,
}
```

### 注册中心 (registry.rs)

```rust
pub struct BrainRegistry {
    active: HashMap<BrainId, ActiveBrainEntry>,
    dormant: HashMap<BrainId, DormantBrainEntry>,
    templates: HashMap<String, BrainTemplate>,
    storage_dir: PathBuf,
}

struct ActiveBrainEntry {
    template: BrainTemplate,
    task_handle: JoinHandle<()>,
    registered_at: DateTime<Utc>,
    task_count: u32,
}

struct DormantBrainEntry {
    template: BrainTemplate,
    dormant_since: DateTime<Utc>,
    original_weight: f64,
}
```

---

## 硬休眠机制 (dormancy.rs)

```rust
pub struct DormancyManager {
    storage_dir: PathBuf,
    dormancy_threshold: f64,  // 默认 0.2
    wake_threshold: f64,      // 默认 0.6
}
```

休眠流程：
1. 主脑每轮任务后检查权重
2. `weight <= 0.2` → 触发休眠
3. 序列化配置到 `~/.ai-brain/brains/{id}.toml`
4. 取消 JoinHandle（终止任务）
5. 从总线注销
6. 对象被 Drop

唤醒流程：
1. 感知脑识别到某类任务与休眠副脑的能力标签匹配
2. 从磁盘加载配置
3. 重新实例化副脑对象
4. 重新注册到总线
5. 初始权重 = 休眠时权重 + 0.1（唤醒奖励）

---

## LLM 建议式创建 (suggestion.rs)

```rust
pub struct CreationSuggestion {
    pub reason: String,
    pub suggested_name: String,
    pub suggested_capabilities: Vec<String>,
    pub parent_brain: Option<BrainId>,
    pub confidence: f64,
}

pub struct SuggestionEngine {
    task_history: Vec<TaskPattern>,
    min_tasks_before_suggest: u32,
    llm_client: Box<dyn LlmProvider>,
}
```

触发流程：
1. 收集最近 N 轮任务的模式
2. 检测：某类子任务反复出现 + 现有副脑置信度不高 + 频率超阈值
3. 模式摘要发给 LLM，输出 CreationSuggestion
4. 返回建议给用户
5. 用户确认 → 从模板实例化 → 注册到总线

---

## CLI/API 集成

CLI 新增命令：
```bash
ai-brain brain list                  # 列出所有副脑（活跃+休眠）
ai-brain brain create <template>     # 从模板创建
ai-brain brain dormant <id>          # 手动休眠
ai-brain brain wake <id>             # 手动唤醒
ai-brain brain suggest               # LLM 建议创建
```

API 新增端点：
```
GET  /api/brains                     # 副脑列表（含状态）
POST /api/brains                     # 创建新副脑
POST /api/brains/:id/dormant         # 休眠
POST /api/brains/:id/wake            # 唤醒
GET  /api/brains/suggest             # 获取创建建议
```
