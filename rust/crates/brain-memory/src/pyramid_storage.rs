//! Per-Persona 金字塔存储层
//!
//! 管理每个人格独立的四层金字塔目录结构。
//! 路径模式: `~/.ai-brain/personas/{persona_id}/pyramid/...`

use crate::error::Result;
use std::fs;
use std::path::{Path, PathBuf};

/// Per-Persona 金字塔存储
#[derive(Clone, Debug)]
pub struct PyramidStorage {
    /// ~/.ai-brain/
    base_dir: PathBuf,
    /// 当前激活的人格 ID
    persona_id: String,
}

impl PyramidStorage {
    /// 创建 PyramidStorage 实例
    pub fn new(base_dir: PathBuf, persona_id: impl Into<String>) -> Self {
        Self {
            base_dir,
            persona_id: persona_id.into(),
        }
    }

    /// 切换人格（返回新的 PyramidStorage）
    pub fn for_persona(&self, persona_id: &str) -> Self {
        Self {
            base_dir: self.base_dir.clone(),
            persona_id: persona_id.to_string(),
        }
    }

    /// 获取人格 ID
    pub fn persona_id(&self) -> &str {
        &self.persona_id
    }

    /// 获取存储根目录
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    // === 金字塔根目录 ===

    /// 人格金字塔根目录: `personas/{persona_id}/pyramid/`
    pub fn pyramid_root(&self) -> PathBuf {
        self.base_dir
            .join("personas")
            .join(&self.persona_id)
            .join("pyramid")
    }

    // === L1 全量记忆基座 ===

    /// L1 目录: `personas/{persona_id}/pyramid/l1-raw/`
    pub fn l1_dir(&self) -> PathBuf {
        self.pyramid_root().join("l1-raw")
    }

    /// L1 会话文件: `personas/{persona_id}/pyramid/l1-raw/{session_id}.jsonl`
    pub fn l1_session_path(&self, session_id: &str) -> PathBuf {
        self.l1_dir().join(format!("{session_id}.jsonl"))
    }

    // === L2 记忆摘要池 ===

    /// L2 目录: `personas/{persona_id}/pyramid/l2-summary/`
    pub fn l2_dir(&self) -> PathBuf {
        self.pyramid_root().join("l2-summary")
    }

    /// L2 索引文件: `personas/{persona_id}/pyramid/l2-summary/index.json`
    pub fn l2_index_path(&self) -> PathBuf {
        self.l2_dir().join("index.json")
    }

    /// L2 任务文件: `personas/{persona_id}/pyramid/l2-summary/{task_id}.json`
    pub fn l2_task_path(&self, task_id: &str) -> PathBuf {
        self.l2_dir().join(format!("{task_id}.json"))
    }

    // === L3 经验抽象层 ===

    /// L3 目录: `personas/{persona_id}/pyramid/l3-abstract/`
    pub fn l3_dir(&self) -> PathBuf {
        self.pyramid_root().join("l3-abstract")
    }

    /// L3 索引文件: `personas/{persona_id}/pyramid/l3-abstract/index.json`
    pub fn l3_index_path(&self) -> PathBuf {
        self.l3_dir().join("index.json")
    }

    /// L3 类型经验文件: `personas/{persona_id}/pyramid/l3-abstract/{type_name}.json`
    pub fn l3_type_path(&self, type_name: &str) -> PathBuf {
        self.l3_dir().join(format!("{type_name}.json"))
    }

    // === L4 潜意识层 ===

    /// L4 文件: `personas/{persona_id}/pyramid/l4-subconscious.json`
    pub fn l4_path(&self) -> PathBuf {
        self.pyramid_root().join("l4-subconscious.json")
    }

    // === Profile + EvalInfo ===

    /// 人格目录: `personas/{persona_id}/`
    pub fn persona_dir(&self) -> PathBuf {
        self.base_dir.join("personas").join(&self.persona_id)
    }

    /// 用户画像: `personas/{persona_id}/profile.json`
    pub fn profile_path(&self) -> PathBuf {
        self.persona_dir().join("profile.json")
    }

    /// 评估信息: `personas/{persona_id}/eval-info.json`
    pub fn eval_info_path(&self) -> PathBuf {
        self.persona_dir().join("eval-info.json")
    }

    /// 待分析数据: `personas/{persona_id}/pending.json`
    pub fn pending_path(&self) -> PathBuf {
        self.persona_dir().join("pending.json")
    }

    // === 文件操作 ===

    /// 读取 JSON 文件
    pub fn read_json<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<T> {
        let data = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    /// 写入 JSON 文件（pretty print）
    pub fn write_json<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(value)?;
        fs::write(path, data)?;
        Ok(())
    }

    /// 追加一行 JSONL
    pub fn append_jsonl<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(line.as_bytes())?;
        Ok(())
    }

    /// 读取 JSONL 文件所有行
    pub fn read_jsonl<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<Vec<T>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let data = fs::read_to_string(path)?;
        data.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    /// 列出目录下所有 .json 文件
    pub fn list_json_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "json" && e.file_name() != "index.json")
            })
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(files)
    }

    /// 列出目录下所有 .jsonl 文件
    pub fn list_jsonl_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(files)
    }

    /// 确保金字塔目录结构存在
    pub fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            self.l1_dir(),
            self.l2_dir(),
            self.l3_dir(),
            // l4 是单文件，parent = pyramid_root
            self.pyramid_root(),
        ];
        for dir in dirs {
            fs::create_dir_all(&dir)?;
        }
        Ok(())
    }

    /// 清空目录内容（保留目录本身）
    pub fn clean_dir(&self, dir: &Path) -> Result<()> {
        if dir.exists() {
            for entry in fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    fs::remove_file(&path)?;
                }
            }
        }
        Ok(())
    }

    /// 加载 JSON 文件，不存在返回 None
    pub fn read_json_optional<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Result<Option<T>> {
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read_to_string(path)?;
        Ok(Some(serde_json::from_str(&data)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_persona_isolated() {
        let store_a = PyramidStorage::new(PathBuf::from("/tmp/brain"), "cyber-brain");
        let store_b = PyramidStorage::new(PathBuf::from("/tmp/brain"), "writer");

        // L1 路径隔离
        assert!(store_a.l1_dir().to_str().unwrap().contains("cyber-brain"));
        assert!(store_b.l1_dir().to_str().unwrap().contains("writer"));

        // L4 路径隔离
        assert_ne!(store_a.l4_path(), store_b.l4_path());

        // Profile 隔离
        assert_ne!(store_a.profile_path(), store_b.profile_path());
    }

    #[test]
    fn for_persona_creates_new_storage() {
        let store = PyramidStorage::new(PathBuf::from("/tmp/brain"), "default");
        let store2 = store.for_persona("writer");
        assert_eq!(store.persona_id(), "default");
        assert_eq!(store2.persona_id(), "writer");
    }

    #[test]
    fn ensure_dirs_creates_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        store.ensure_dirs().unwrap();

        assert!(store.l1_dir().exists());
        assert!(store.l2_dir().exists());
        assert!(store.l3_dir().exists());
        assert!(store.pyramid_root().exists());
    }

    #[test]
    fn write_and_read_json() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        let path = store.l4_path();
        let data = vec!["hello", "world"];

        store.write_json(&path, &data).unwrap();
        let loaded: Vec<String> = store.read_json(&path).unwrap();
        assert_eq!(loaded, vec!["hello", "world"]);
    }

    #[test]
    fn read_json_optional_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        let path = store.l4_path();
        let result: Option<Vec<String>> = store.read_json_optional(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn append_and_read_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        store.ensure_dirs().unwrap();

        let path = store.l1_session_path("sess-001");
        store.append_jsonl(&path, &"line1").unwrap();
        store.append_jsonl(&path, &"line2").unwrap();

        let lines: Vec<String> = store.read_jsonl(&path).unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
    }

    #[test]
    fn clean_dir_removes_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
        store.ensure_dirs().unwrap();

        // 写几个文件
        store
            .write_json(&store.l2_task_path("t-1"), &"old")
            .unwrap();
        store
            .write_json(&store.l2_task_path("t-2"), &"old")
            .unwrap();
        assert_eq!(store.list_json_files(&store.l2_dir()).unwrap().len(), 2);

        // 清空
        store.clean_dir(&store.l2_dir()).unwrap();
        assert_eq!(store.list_json_files(&store.l2_dir()).unwrap().len(), 0);
    }

    #[test]
    fn l1_session_path_format() {
        let store = PyramidStorage::new(PathBuf::from("/tmp/brain"), "cyber-brain");
        let path = store.l1_session_path("sess-abc123");
        assert_eq!(
            path,
            PathBuf::from("/tmp/brain/personas/cyber-brain/pyramid/l1-raw/sess-abc123.jsonl")
        );
    }

    #[test]
    fn profile_and_eval_info_paths() {
        let store = PyramidStorage::new(PathBuf::from("/tmp/brain"), "writer");
        assert_eq!(
            store.profile_path(),
            PathBuf::from("/tmp/brain/personas/writer/profile.json")
        );
        assert_eq!(
            store.eval_info_path(),
            PathBuf::from("/tmp/brain/personas/writer/eval-info.json")
        );
    }
}
