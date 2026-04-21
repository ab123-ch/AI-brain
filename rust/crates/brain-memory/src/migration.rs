//! 数据迁移模块 — 旧四层金字塔 → 新三层架构
//!
//! 迁移逻辑：
//! 1. L3 Raw → L1 Raw：文件格式不变，新字段有 serde(default) 自动填充
//! 2. L2 ShortTerm + L1 EventIndex → L2 IndexEntry：合并转换
//! 3. L0 TaskSummary → L3 ExperiencePack：字段映射

use std::path::Path;

use crate::error::Result;
use crate::experience_pack::{ExperiencePack, ExperiencePackLayer};
use crate::index_layer::IndexLayer;
use crate::index_layer::{IndexEntry, SourceRef};
use crate::storage::Storage;

/// 执行迁移
pub fn run_migration(base_dir: &Path) -> Result<MigrationReport> {
    let storage = Storage::new_lazy(base_dir.to_path_buf());
    let mut report = MigrationReport::default();

    // 检查是否需要迁移（旧目录存在且新迁移标记不存在）
    let migration_marker = base_dir.join("memory").join(".migrated_v2");
    if migration_marker.exists() {
        tracing::info!("迁移已完成，跳过");
        return Ok(report);
    }

    let has_old_data = storage.short_term_dir().exists()
        || storage.events_dir().exists()
        || storage.tasks_dir().exists();

    if !has_old_data {
        // 没有旧数据，直接写标记
        std::fs::write(&migration_marker, "v2")?;
        return Ok(report);
    }

    tracing::info!("开始旧格式数据迁移...");

    // 创建新层
    let index = IndexLayer::new(Storage::new_lazy(base_dir.to_path_buf()));
    let experience = ExperiencePackLayer::new(Storage::new_lazy(base_dir.to_path_buf()));

    // Step 1: 迁移 TaskSummary → ExperiencePack
    if storage.tasks_dir().exists() {
        let files = storage.list_json_files(&storage.tasks_dir())?;
        for file in &files {
            match migrate_task_summary(&experience, file) {
                Ok(()) => report.task_summaries_migrated += 1,
                Err(e) => {
                    tracing::warn!("迁移 TaskSummary 失败 {:?}: {e}", file.file_name());
                    report.errors += 1;
                }
            }
        }
    }

    // Step 2: 迁移 EventIndex → IndexEntry
    if storage.events_dir().exists() {
        let files = storage.list_json_files(&storage.events_dir())?;
        for file in &files {
            match migrate_event_entries(&index, file) {
                Ok(count) => report.event_entries_migrated += count,
                Err(e) => {
                    tracing::warn!("迁移 EventEntry 失败 {:?}: {e}", file.file_name());
                    report.errors += 1;
                }
            }
        }
    }

    // 写迁移标记
    std::fs::write(&migration_marker, "v2")?;
    tracing::info!(
        "迁移完成: {} 个任务总结, {} 个事件索引, {} 个错误",
        report.task_summaries_migrated,
        report.event_entries_migrated,
        report.errors
    );

    Ok(report)
}

fn migrate_task_summary(experience: &ExperiencePackLayer, file: &std::path::Path) -> Result<()> {
    let old: crate::task_summary::TaskSummary =
        serde_json::from_str(&std::fs::read_to_string(file)?)?;

    let pack = ExperiencePack {
        id: old.id,
        title: old.task_description,
        category: "general".into(),
        trigger_patterns: old.trigger_pattern.split(',').map(String::from).collect(),
        reasoning_path: old.reasoning_path,
        mistakes: old
            .mistakes
            .into_iter()
            .map(|m| crate::experience_pack::MistakeEntry {
                what: m.what,
                why: m.why,
                how_to_avoid: m.how_to_avoid,
            })
            .collect(),
        files_modified_patterns: old.files_modified,
        tools_used: old.tools_used,
        success_rate: old.success_rate,
        shortcuts: old.shortcuts,
        source_index_ids: Vec::new(),
        context_snippet: String::new(),
        created_at: old.created_at,
        last_used_at: old.created_at,
        use_count: 0,
    };

    experience.store(&pack)
}

fn migrate_event_entries(index: &IndexLayer, file: &std::path::Path) -> Result<u32> {
    let events: Vec<crate::event_index::EventEntry> =
        serde_json::from_str(&std::fs::read_to_string(file)?)?;
    let mut count = 0u32;

    for event in &events {
        let entry = IndexEntry {
            id: event.id.clone(),
            summary: event.summary.clone(),
            category: categorize_by_tags(&event.tags),
            tags: event.tags.clone(),
            importance: event.importance,
            source_refs: vec![SourceRef {
                session_file: "migrated".into(),
                line_range: (0, 0),
                entry_ids: Vec::new(),
            }],
            created_at: event.created_at,
            last_accessed: event.created_at,
            recall_count: 0,
        };

        index.store(&entry)?;
        count += 1;
    }

    Ok(count)
}

/// 根据 tags 推断分类
fn categorize_by_tags(tags: &[String]) -> String {
    let tag_str = tags.join(" ").to_lowercase();
    if tag_str.contains("bug") || tag_str.contains("错误") || tag_str.contains("调试") {
        "debugging".into()
    } else if tag_str.contains("架构") || tag_str.contains("设计") {
        "architecture".into()
    } else if tag_str.contains("测试") {
        "testing".into()
    } else if tag_str.contains("部署") || tag_str.contains("发布") {
        "deployment".into()
    } else {
        "development".into()
    }
}

#[derive(Debug, Default)]
pub struct MigrationReport {
    pub task_summaries_migrated: u32,
    pub event_entries_migrated: u32,
    pub errors: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorize_by_tags_works() {
        assert_eq!(categorize_by_tags(&["bug".into()]), "debugging");
        assert_eq!(categorize_by_tags(&["架构".into()]), "architecture");
        assert_eq!(categorize_by_tags(&["开发".into()]), "development");
    }
}
