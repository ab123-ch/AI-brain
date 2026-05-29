//! 配置管理器
//!
//! 管理 ~/.ai-brain/config.toml 的读写操作

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// 配置管理器
pub struct ConfigManager {
    config_path: PathBuf,
    config: BTreeMap<String, String>,
}

impl ConfigManager {
    /// 创建配置管理器
    pub fn new() -> Self {
        let config_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".ai-brain")
            .join("config.toml");

        let config = Self::load_config(&config_path).unwrap_or_default();

        Self {
            config_path,
            config,
        }
    }

    /// 获取配置项
    pub fn get(&self, key: &str) -> Option<&String> {
        self.config.get(key)
    }

    /// 设置配置项
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.config.insert(key.to_string(), value.to_string());
        self.save_config()
    }

    /// 获取所有配置
    pub fn all(&self) -> &BTreeMap<String, String> {
        &self.config
    }

    /// 加载配置文件
    fn load_config(path: &PathBuf) -> Result<BTreeMap<String, String>, String> {
        if !path.exists() {
            return Ok(BTreeMap::new());
        }

        let content = fs::read_to_string(path)
            .map_err(|e| format!("读取配置文件失败: {e}"))?;

        // 简单解析 TOML 格式的 key = "value"
        let mut config = BTreeMap::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_string();
                let value = value.trim().trim_matches('"').to_string();
                config.insert(key, value);
            }
        }

        Ok(config)
    }

    /// 保存配置文件
    fn save_config(&self) -> Result<(), String> {
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建配置目录失败: {e}"))?;
        }

        let content = self
            .config
            .iter()
            .map(|(k, v)| format!("{k} = \"{v}\""))
            .collect::<Vec<_>>()
            .join("\n");

        fs::write(&self.config_path, content)
            .map_err(|e| format!("写入配置文件失败: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_manager() {
        let mut mgr = ConfigManager::new();
        mgr.set("test_key", "test_value").unwrap();
        assert_eq!(mgr.get("test_key"), Some(&"test_value".to_string()));
    }
}
