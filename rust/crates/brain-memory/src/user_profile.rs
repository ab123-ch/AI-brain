//! 用户画像存储与更新（v2 四步分析第二步产出）
//!
//! 职责：
//! - 持久化用户画像到 `memory/profile/user_profile.json`
//! - 加载已有画像
//! - 合并 LLM 分析结果到现有画像（去重 + 追加）
//! - 提供 `UserProfile` 的完整生命周期管理

use std::path::Path;

use chrono::Utc;

use brain_core::types::UserProfile;

use crate::error::Result;
use crate::storage::Storage;

/// 用户画像存储管理器
pub struct UserProfileStore {
    storage: Storage,
}

impl UserProfileStore {
    /// 创建用户画像存储管理器
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// 获取存储根目录（用于 CloneStore trait）
    pub fn base_dir(&self) -> &Path {
        self.storage.base_dir()
    }

    /// 获取画像文件路径
    fn profile_path(&self) -> std::path::PathBuf {
        self.storage
            .base_dir()
            .join("memory")
            .join("profile")
            .join("user_profile.json")
    }

    /// 加载用户画像，不存在则返回默认值
    pub fn load(&self) -> Result<UserProfile> {
        let path = self.profile_path();
        if !path.exists() {
            return Ok(UserProfile::default());
        }
        self.storage.read_json(&path)
    }

    /// 保存完整用户画像（原子写入）
    pub fn save(&self, profile: &UserProfile) -> Result<()> {
        let path = self.profile_path();
        self.storage.write_json_atomic(&path, profile)
    }

    /// 将 LLM 分析结果合并到现有画像
    ///
    /// 合并策略：
    /// - 新条目去重后追加到对应字段
    /// - 禁忌(taboos)永不删除，只追加
    /// - 习惯(habits)取并集
    /// - 更新 updated_at 时间戳
    pub fn merge_analysis(
        &self,
        explicit_prefs: &[String],
        implicit_prefs: &[String],
        new_taboos: &[String],
        new_habits: &[String],
    ) -> Result<UserProfile> {
        let mut profile = self.load()?;

        merge_dedup(&mut profile.explicit_preferences, explicit_prefs);
        merge_dedup(&mut profile.implicit_preferences, implicit_prefs);
        merge_dedup(&mut profile.taboos, new_taboos);
        merge_dedup(&mut profile.habits, new_habits);

        profile.updated_at = Utc::now();
        self.save(&profile)?;
        Ok(profile)
    }
}

/// 将新条目合并到已有列表中（去重，大小写不敏感）
fn merge_dedup(existing: &mut Vec<String>, incoming: &[String]) {
    let mut seen: std::collections::HashSet<String> =
        existing.iter().map(|s| s.to_lowercase()).collect();

    for item in incoming {
        let trimmed = item.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            existing.push(trimmed.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, UserProfileStore) {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();
        (tmp, UserProfileStore::new(storage))
    }

    #[test]
    fn load_default_when_missing() {
        let (_tmp, store) = make_store();
        let profile = store.load().unwrap();
        assert!(profile.explicit_preferences.is_empty());
        assert!(profile.taboos.is_empty());
    }

    #[test]
    fn save_and_reload() {
        let (_tmp, store) = make_store();
        let mut profile = UserProfile::default();
        profile.explicit_preferences.push("使用 Rust".into());
        profile.taboos.push("不要使用 unwrap".into());

        store.save(&profile).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded.explicit_preferences, vec!["使用 Rust"]);
        assert_eq!(loaded.taboos, vec!["不要使用 unwrap"]);
    }

    #[test]
    fn merge_analysis_deduplicates() {
        let (_tmp, store) = make_store();

        // 首次合并
        let profile = store
            .merge_analysis(
                &["Rust".into()],
                &["喜欢简洁代码".into()],
                &["不要 GC".into()],
                &["TDD".into()],
            )
            .unwrap();
        assert_eq!(profile.explicit_preferences.len(), 1);

        // 再次合并，带重复项
        let profile = store
            .merge_analysis(
                &["Rust".into(), "Tokio".into()],
                &["喜欢简洁代码".into(), "偏好函数式".into()],
                &[],
                &[],
            )
            .unwrap();

        // 去重：Rust 已存在，Tokio 新增
        assert_eq!(profile.explicit_preferences.len(), 2);
        assert!(profile.explicit_preferences.contains(&"Rust".to_string()));
        assert!(profile.explicit_preferences.contains(&"Tokio".to_string()));

        // 隐性偏好：去重
        assert_eq!(profile.implicit_preferences.len(), 2);

        // 禁忌保留
        assert_eq!(profile.taboos.len(), 1);
    }

    #[test]
    fn merge_ignores_empty_strings() {
        let (_tmp, store) = make_store();
        let profile = store
            .merge_analysis(&[String::new(), "  ".into()], &[], &[], &[])
            .unwrap();
        assert!(profile.explicit_preferences.is_empty());
    }
}
