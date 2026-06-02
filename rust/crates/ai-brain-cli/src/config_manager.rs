//! 配置管理器
//!
//! 管理 ~/.ai-brain/config.toml 的读写操作。
//! 使用 toml::Value 保持完整的 TOML 文档结构（section 嵌套），
//! 避免写入时丢失 [llm]、[llm.providers.xxx] 等 section 头。

use std::fs;
use std::path::PathBuf;

/// 配置管理器
pub struct ConfigManager {
    config_path: PathBuf,
    doc: toml::Value,
}

impl ConfigManager {
    /// 创建配置管理器
    pub fn new() -> Self {
        let config_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ai-brain")
            .join("config.toml");

        let doc = Self::load_doc(&config_path);
        Self { config_path, doc }
    }

    /// 获取配置项（支持点分路径，如 "llm.default_provider"）
    pub fn get(&self, key: &str) -> Option<String> {
        Self::get_nested(&self.doc, key)
    }

    /// 设置配置项（支持点分路径，自动创建中间表）
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        Self::set_nested(&mut self.doc, key, value);
        self.save_doc()
    }

    /// 获取所有扁平化的配置项（用于展示）
    pub fn all(&self) -> Vec<(String, String)> {
        let mut result = Vec::new();
        Self::flatten(&self.doc, String::new(), &mut result);
        result
    }

    // ── 内部方法 ──────────────────────────────────────────

    fn load_doc(path: &PathBuf) -> toml::Value {
        if !path.exists() {
            return toml::Value::Table(toml::map::Map::new());
        }
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return toml::Value::Table(toml::map::Map::new()),
        };
        content.parse::<toml::Value>().unwrap_or(toml::Value::Table(toml::map::Map::new()))
    }

    fn save_doc(&self) -> Result<(), String> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建配置目录失败: {e}"))?;
        }

        let content = toml::to_string_pretty(&self.doc)
            .map_err(|e| format!("序列化配置失败: {e}"))?;

        fs::write(&self.config_path, content)
            .map_err(|e| format!("写入配置文件失败: {e}"))?;

        Ok(())
    }

    /// 按点分路径读取嵌套值
    fn get_nested(doc: &toml::Value, key: &str) -> Option<String> {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = doc;
        for part in &parts[..parts.len().saturating_sub(1)] {
            current = current.as_table()?.get(*part)?;
        }
        let leaf_key = parts.last()?;
        let leaf = current.as_table()?.get(*leaf_key)?;
        Some(match leaf {
            toml::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }

    /// 按点分路径写入嵌套值（自动创建中间表）
    fn set_nested(doc: &mut toml::Value, key: &str, value: &str) {
        let parts: Vec<&str> = key.split('.').collect();
        if parts.is_empty() {
            return;
        }

        // 确保 doc 是 Table
        if !doc.is_table() {
            *doc = toml::Value::Table(toml::map::Map::new());
        }

        let mut current = doc.as_table_mut().unwrap();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current = current
                .entry(part.to_string())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
        }

        let leaf_key = parts.last().unwrap();
        // 尝试解析为数字/布尔，否则当字符串
        let val = if let Ok(b) = value.parse::<bool>() {
            toml::Value::Boolean(b)
        } else if let Ok(i) = value.parse::<i64>() {
            toml::Value::Integer(i)
        } else if let Ok(f) = value.parse::<f64>() {
            toml::Value::Float(f)
        } else {
            toml::Value::String(value.to_string())
        };
        current.insert(leaf_key.to_string(), val);
    }

    /// 将嵌套 TOML 值扁平化为 key=value 列表
    fn flatten(val: &toml::Value, prefix: String, out: &mut Vec<(String, String)>) {
        if let Some(table) = val.as_table() {
            for (k, v) in table {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match v {
                    toml::Value::Table(_) => Self::flatten(v, key, out),
                    toml::Value::String(s) => out.push((key, s.clone())),
                    other => out.push((key, other.to_string())),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_config_roundtrip_preserves_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // 写入一个带 section 的 TOML
        let original = r#"
[llm]
default_provider = "zhipu"
default_model = "glm-4.7"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

[llm.brain_models]
reasoning = "glm-5.1"
"#;
        fs::write(&path, original).unwrap();

        // 用 ConfigManager 读取 → 设置新值 → 保存
        let mut mgr = ConfigManager::new();
        // 手动覆盖路径用于测试
        let mut mgr = ConfigManager {
            config_path: path.clone(),
            doc: ConfigManager::load_doc(&path),
        };
        mgr.set("llm.default_model", "glm-5.1").unwrap();

        // 重新读取验证 section 结构完好
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("[llm]"),
            "should preserve [llm] section, got:\n{content}"
        );
        assert!(
            content.contains("[llm.providers.zhipu]"),
            "should preserve [llm.providers.zhipu] section, got:\n{content}"
        );
        assert!(
            content.contains("[llm.brain_models]"),
            "should preserve [llm.brain_models] section, got:\n{content}"
        );
        assert!(
            content.contains("\"glm-5.1\""),
            "should have updated model, got:\n{content}"
        );
    }

    #[test]
    fn test_get_nested() {
        let mut mgr = ConfigManager::new();
        // set_nested 然后立即 get
        ConfigManager::set_nested(&mut mgr.doc, "llm.default_model", "test-model");
        assert_eq!(mgr.get("llm.default_model"), Some("test-model".to_string()));
    }

    #[test]
    fn test_all_flatten() {
        let mut mgr = ConfigManager::new();
        ConfigManager::set_nested(&mut mgr.doc, "llm.default_model", "glm-5.1");
        ConfigManager::set_nested(&mut mgr.doc, "llm.defaults.max_tokens", "8192");
        let all = mgr.all();
        assert!(all.iter().any(|(k, v)| k == "llm.default_model" && v == "glm-5.1"));
        assert!(all.iter().any(|(k, v)| k == "llm.defaults.max_tokens" && v == "8192"));
    }
}
