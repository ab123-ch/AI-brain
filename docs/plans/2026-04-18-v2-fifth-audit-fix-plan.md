# v2 第五轮全量审计问题清单

> 审计时间: 2026-04-18
> 审计范围: brain-core, brain-llm, brain-main, brain-eval, brain-memory, ai-brain-cli, tools
> 审计方法: 逐方法检查参数使用/返回值/空实现/死代码/import/字段读写
> 修复时间: 待定

---

## P1 (中) — 死配置 / 可见性

- [ ] R-P1-1: `brain-core/config.rs:83-84` — `MemorySection` 删除 max_keywords 后变成空结构体 `{}`，全项目无代码读取它
- [ ] R-P1-2: `brain-core/config.rs:57-66` — `ThresholdConfig` 4 个字段有 3 个是死配置：`context_warning_threshold`/`memory_analysis_interval`/`consolidation_importance` 无代码读取，只有 `context_danger_threshold` 被使用
- [ ] R-P1-3: `brain-llm/openai_compat.rs:61,71` — `ApiMessage.role` 和 `ApiToolCall.r#type` 标注 `#[allow(dead_code)]`，应加注释说明是 serde 反序列化所需

---

## P2 (低) — 功能 Bug / 数据完整性

- [ ] R-P2-1: `brain-memory/recall.rs:109-113` — **recall 计数永远不生效** — `increment_use(id, "")` 传空 category，拼接路径查找 `"".json` 必然失败，`let _ =` 静默吞错。recall_count/use_count 永远为 0
- [ ] R-P2-2: `brain-memory/consolidation.rs:493-496` — `source_refs` 的 `session_file` 硬编码为 `"consolidated"` 而非实际 session_id，L2→L1 反向索引链断裂
- [ ] R-P2-3: `brain-memory/index_layer.rs:42` — `IndexEntry.line_range` 全局只写 `(0,0)` 或 `(0,n)`，从未被读取
- [ ] R-P2-4: `brain-memory/memory_brain.rs:296` — `store_rich_record` 的 `raw_input` 与 `content` 参数所有调用者都传相同值，功能重复

---

## 统计

| 级别 | 数量 |
|------|------|
| P1 | 3 |
| P2 | 4 |
| **总计** | **7** |

---

## 历史轮次累计

| 轮次 | 新增 | 已修复 | 延后 |
|------|------|--------|------|
| 第三轮 | 45 | 13 | 32 |
| 第四轮 | 39 | 15 | 24 |
| 第五轮 | 7 | 0 | 7 |
