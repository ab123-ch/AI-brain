use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// 文件系统存储抽象
///
/// 管理记忆文件的读写，确保目录结构存在。
#[derive(Clone)]
pub struct Storage {
    base_dir: PathBuf,
}

/// 将 topic 名称转化为安全的目录名（去特殊字符，截断长度）
fn sanitize_topic_name(topic: &str) -> String {
    let sanitized: String = topic
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.len() > 100 {
        sanitized[..100].to_string()
    } else {
        sanitized
    }
}

impl Storage {
    /// 创建存储实例，自动创建目录结构
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        let storage = Self { base_dir };
        storage.ensure_dirs()?;
        Ok(storage)
    }

    /// 预览路径但不创建目录（用于测试）
    pub fn new_lazy(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    // === 目录路径 ===

    /// L3 原始记忆: `sessions/`
    pub fn sessions_dir(&self) -> PathBuf {
        self.base_dir.join("sessions")
    }

    /// L2 短期记忆: `memory/short-term/`
    pub fn short_term_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("short-term")
    }

    /// L1 事件索引: `memory/events/`
    pub fn events_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("events")
    }

    /// L0 任务总结: `memory/long-term/tasks/`
    pub fn tasks_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("long-term").join("tasks")
    }

    /// L2 会话总结: `memory/summaries/`
    pub fn summaries_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("summaries")
    }

    /// L1 归档根目录: `memory/archive/`
    pub fn archive_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("archive")
    }

    /// L1 归档分类目录: `memory/archive/{topic}/`
    pub fn archive_topic_dir(&self, topic: &str) -> PathBuf {
        self.archive_dir().join(sanitize_topic_name(topic))
    }

    /// 关键词索引: `memory/index/tags.json`
    pub fn tags_index_path(&self) -> PathBuf {
        self.base_dir.join("memory").join("index").join("tags.json")
    }

    /// 获取存储根目录
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
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
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .map(|e| e.path())
            .collect();
        files.sort();
        Ok(files)
    }

    /// 原子写入 JSON 文件（先写临时文件再 rename）
    pub fn write_json_atomic<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        let data = serde_json::to_string_pretty(value)?;
        fs::write(&tmp_path, &data)?;
        fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// 递归列出目录及子目录下所有 .json 文件
    pub fn list_json_files_recursive(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        self.collect_json_files_recursive(dir, &mut files)?;
        files.sort();
        Ok(files)
    }

    fn collect_json_files_recursive(&self, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.collect_json_files_recursive(&path, files)?;
            } else if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
        Ok(())
    }

    /// L2 索引根目录: `memory/index/`
    pub fn index_dir(&self) -> PathBuf {
        self.base_dir.join("memory").join("index")
    }

    /// L2 索引分类目录: `memory/index/{category}/`
    pub fn index_category_dir(&self, category: &str) -> PathBuf {
        self.base_dir.join("memory").join("index").join(category)
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

    /// 确保所有目录存在
    fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            self.sessions_dir(),
            self.short_term_dir(),
            self.events_dir(),
            self.tasks_dir(),
            self.tags_index_path().parent().unwrap().to_path_buf(),
        ];
        for dir in dirs {
            fs::create_dir_all(&dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ensure_dirs_creates_structure() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        assert!(storage.sessions_dir().exists());
        assert!(storage.short_term_dir().exists());
        assert!(storage.events_dir().exists());
        assert!(storage.tasks_dir().exists());
    }

    #[test]
    fn write_and_read_json() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        let path = storage.short_term_dir().join("test.json");
        let data = vec!["hello", "world"];
        storage.write_json(&path, &data).unwrap();

        let loaded: Vec<String> = storage.read_json(&path).unwrap();
        assert_eq!(loaded, vec!["hello", "world"]);
    }

    #[test]
    fn append_and_read_jsonl() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf()).unwrap();

        let path = storage.sessions_dir().join("session.jsonl");
        storage.append_jsonl(&path, &"line1").unwrap();
        storage.append_jsonl(&path, &"line2").unwrap();

        let lines: Vec<String> = storage.read_jsonl(&path).unwrap();
        assert_eq!(lines, vec!["line1", "line2"]);
    }
}
