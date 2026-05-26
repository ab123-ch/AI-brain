// === 新金字塔模块（活跃） ===
pub mod abstract_layer;
pub mod concentration;
pub mod error;
pub mod persona_manager;
pub mod persona_types;
pub mod profile_eval;
pub mod progressive_recall;
pub mod prompts;
pub mod pyramid_memory_brain;
pub mod pyramid_storage;
pub mod pyramid_types;
pub mod raw_pool;
pub mod summary_pool;
pub mod subconscious_pool;

// === 旧模块保留（无交叉依赖，可编译） ===
pub mod archive;
pub mod eval_requirement;
pub mod evolution;
pub mod pending_analysis;
pub mod pitfall;
pub mod raw_layer;
pub mod storage;
pub mod subconscious;
pub mod summary;
pub mod user_profile;

// === 旧模块已禁用（Task 16: 依赖链断裂） ===
// 如需回滚，取消注释以下行并恢复对应 .rs 文件
// pub mod analyzer;           // → 依赖 index_layer, memory_iteration
// pub mod brain_state;        // → 依赖 index_layer
// pub mod consolidation;      // → 依赖 event_index, short_term, task_summary
// pub mod event_index;        // → 已删除
// pub mod guardian;           // → 依赖 consolidation, importance, analyzer
// pub mod importance;         // → 依赖 memory_iteration
// pub mod index_layer;        // → 已删除
// pub mod memory_brain;       // → 依赖 consolidation, event_index, short_term, task_summary, analyzer, importance
// pub mod memory_iteration;   // → 已删除
// pub mod recall;             // → 依赖 event_index, short_term, task_summary
// pub mod short_term;         // → 已删除
// pub mod task_summary;       // → 已删除
