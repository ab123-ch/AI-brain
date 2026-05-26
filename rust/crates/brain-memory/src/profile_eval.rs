//! Profile + EvalInfo 融合知识层
//!
//! Profile: 100字以内的用户画像摘要，全量重生成
//! EvalInfo: 为评估脑提供的评估信息（requirements + pitfalls + rules）
//! 路径: `personas/{persona_id}/profile.json` 和 `eval-info.json`

use chrono::Utc;
use crate::error::Result;
use crate::pyramid_storage::PyramidStorage;
use crate::pyramid_types::{EvalInfo, PersonaProfile};

/// 用户画像存储（100字上限）
pub struct ProfileStore {
    storage: PyramidStorage,
}

impl ProfileStore {
    /// 创建 ProfileStore
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 加载用户画像
    pub fn load(&self) -> Result<Option<PersonaProfile>> {
        let path = self.storage.profile_path();
        self.storage.read_json_optional(&path)
    }

    /// 全量覆盖重写画像
    pub fn regenerate(&self, summary: &str) -> Result<()> {
        let profile = PersonaProfile {
            summary: summary.to_string(),
            updated_at: Utc::now(),
        };
        self.storage.write_json(&self.storage.profile_path(), &profile)
    }

    /// 获取画像文本（用于注入）
    pub fn summary(&self) -> Result<String> {
        let profile = self.load()?;
        Ok(profile.map(|p| p.summary).unwrap_or_default())
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

/// 评估信息存储
pub struct EvalInfoStore {
    storage: PyramidStorage,
}

impl EvalInfoStore {
    /// 创建 EvalInfoStore
    pub fn new(storage: PyramidStorage) -> Self {
        Self { storage }
    }

    /// 加载评估信息
    pub fn load(&self) -> Result<Option<EvalInfo>> {
        let path = self.storage.eval_info_path();
        self.storage.read_json_optional(&path)
    }

    /// 全量覆盖重写评估信息
    pub fn regenerate(
        &self,
        requirements: Vec<String>,
        pitfalls: Vec<String>,
        rules: Vec<String>,
    ) -> Result<()> {
        let info = EvalInfo {
            requirements,
            pitfalls,
            rules,
            updated_at: Utc::now(),
        };
        self.storage
            .write_json(&self.storage.eval_info_path(), &info)
    }

    /// 获取注入文本（用于评估脑上下文）
    pub fn inject_text(&self) -> Result<String> {
        let Some(info) = self.load()? else {
            return Ok(String::new());
        };

        let mut parts = Vec::new();
        if !info.requirements.is_empty() {
            parts.push(format!(
                "[评估要求]\n{}",
                info.requirements.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
            ));
        }
        if !info.pitfalls.is_empty() {
            parts.push(format!(
                "[已知踩坑]\n{}",
                info.pitfalls.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
            ));
        }
        if !info.rules.is_empty() {
            parts.push(format!(
                "[进化规则]\n{}",
                info.rules.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n")
            ));
        }

        Ok(parts.join("\n\n"))
    }

    /// 获取底层 PyramidStorage 引用
    pub fn storage(&self) -> &PyramidStorage {
        &self.storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stores(persona_id: &str) -> (ProfileStore, EvalInfoStore, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = PyramidStorage::new(tmp.path().to_path_buf(), persona_id);
        storage.ensure_dirs().unwrap();
        (
            ProfileStore::new(storage.clone()),
            EvalInfoStore::new(storage),
            tmp,
        )
    }

    // === Profile 测试 ===

    #[test]
    fn profile_save_and_load() {
        let (profile, _, _tmp) = make_stores("test");
        profile.regenerate("用户是Rust全栈开发者").unwrap();

        let loaded = profile.load().unwrap().unwrap();
        assert_eq!(loaded.summary, "用户是Rust全栈开发者");
    }

    #[test]
    fn profile_regenerate_overwrites() {
        let (profile, _, _tmp) = make_stores("test");
        profile.regenerate("旧画像").unwrap();
        profile.regenerate("新画像").unwrap();

        let loaded = profile.load().unwrap().unwrap();
        assert_eq!(loaded.summary, "新画像");
    }

    #[test]
    fn profile_load_empty() {
        let (profile, _, _tmp) = make_stores("test");
        assert!(profile.load().unwrap().is_none());
    }

    #[test]
    fn profile_summary_text() {
        let (profile, _, _tmp) = make_stores("test");
        profile.regenerate("开发者").unwrap();
        assert_eq!(profile.summary().unwrap(), "开发者");
    }

    #[test]
    fn profile_persona_isolation() {
        let tmp = tempfile::tempdir().unwrap();
        let sa = PyramidStorage::new(tmp.path().to_path_buf(), "a");
        sa.ensure_dirs().unwrap();
        let sb = PyramidStorage::new(tmp.path().to_path_buf(), "b");
        sb.ensure_dirs().unwrap();

        let pa = ProfileStore::new(sa);
        let pb = ProfileStore::new(sb);

        pa.regenerate("A画像").unwrap();
        pb.regenerate("B画像").unwrap();

        assert_eq!(pa.summary().unwrap(), "A画像");
        assert_eq!(pb.summary().unwrap(), "B画像");
    }

    // === EvalInfo 测试 ===

    #[test]
    fn eval_info_save_and_load() {
        let (_, eval, _tmp) = make_stores("test");
        eval.regenerate(
            vec!["不能重复".into()],
            vec!["避免空指针".into()],
            vec!["先测试再提交".into()],
        )
        .unwrap();

        let loaded = eval.load().unwrap().unwrap();
        assert_eq!(loaded.requirements.len(), 1);
        assert_eq!(loaded.pitfalls.len(), 1);
        assert_eq!(loaded.rules.len(), 1);
    }

    #[test]
    fn eval_info_regenerate_overwrites() {
        let (_, eval, _tmp) = make_stores("test");
        eval.regenerate(vec!["旧".into()], vec![], vec![])
            .unwrap();
        eval.regenerate(vec!["新".into()], vec!["坑".into()], vec![])
            .unwrap();

        let loaded = eval.load().unwrap().unwrap();
        assert_eq!(loaded.requirements[0], "新");
        assert_eq!(loaded.pitfalls[0], "坑");
    }

    #[test]
    fn eval_info_load_empty() {
        let (_, eval, _tmp) = make_stores("test");
        assert!(eval.load().unwrap().is_none());
    }

    #[test]
    fn eval_info_inject_text() {
        let (_, eval, _tmp) = make_stores("test");
        eval.regenerate(
            vec!["要测试".into()],
            vec!["别忘判空".into()],
            vec!["TDD优先".into()],
        )
        .unwrap();

        let text = eval.inject_text().unwrap();
        assert!(text.contains("[评估要求]"));
        assert!(text.contains("要测试"));
        assert!(text.contains("[已知踩坑]"));
        assert!(text.contains("别忘判空"));
        assert!(text.contains("[进化规则]"));
        assert!(text.contains("TDD优先"));
    }

    #[test]
    fn eval_info_inject_text_empty() {
        let (_, eval, _tmp) = make_stores("test");
        let text = eval.inject_text().unwrap();
        assert!(text.is_empty());
    }
}
