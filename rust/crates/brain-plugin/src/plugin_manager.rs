use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 插件清单（plugin.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub skills: Option<String>,
    #[serde(default)]
    pub mcp_servers: Option<String>,
    #[serde(default)]
    pub hooks: Option<String>,
}

/// 插件元数据（注册表条目）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub installed_at: String,
    pub source: String,
}

/// 全局注册表（registry.json）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PluginRegistry {
    plugins: HashMap<String, PluginMeta>,
}

/// 插件安装来源
#[derive(Debug, Clone)]
pub enum PluginSource {
    Local { path: PathBuf },
}

/// 插件管理器
pub struct PluginManager {
    registry: PluginRegistry,
    plugins_dir: PathBuf,
}

impl PluginManager {
    /// 加载插件注册表
    pub fn load(plugins_dir: &Path) -> Result<Self, String> {
        let registry_path = plugins_dir.join("registry.json");
        let registry = if registry_path.exists() {
            let content = std::fs::read_to_string(&registry_path)
                .map_err(|e| format!("读取 registry.json 失败: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("解析 registry.json 失败: {e}"))?
        } else {
            PluginRegistry::default()
        };

        Ok(Self {
            registry,
            plugins_dir: plugins_dir.to_path_buf(),
        })
    }

    /// 安装插件（从本地目录）
    pub fn install(&mut self, source: &Path, publisher: &str) -> Result<String, String> {
        let manifest_path = source.join("plugin.json");
        let manifest: PluginManifest = {
            let content = std::fs::read_to_string(&manifest_path)
                .map_err(|e| format!("读取 plugin.json 失败: {e}"))?;
            serde_json::from_str(&content).map_err(|e| format!("解析 plugin.json 失败: {e}"))?
        };

        let version = manifest
            .version
            .clone()
            .unwrap_or_else(|| "0.0.0".to_string());
        let target_dir = self
            .plugins_dir
            .join("cache")
            .join(publisher)
            .join(&manifest.name)
            .join(&version);

        copy_dir_recursive(source, &target_dir).map_err(|e| format!("复制插件文件失败: {e}"))?;

        let meta = PluginMeta {
            name: manifest.name.clone(),
            publisher: publisher.to_string(),
            version,
            installed_at: now_timestamp(),
            source: format!("local:{}", source.display()),
        };
        self.registry.plugins.insert(manifest.name.clone(), meta);
        self.save_registry()?;

        Ok(manifest.name)
    }

    /// 卸载插件
    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let meta = self
            .registry
            .plugins
            .remove(name)
            .ok_or_else(|| format!("插件 '{name}' 未安装"))?;

        let plugin_dir = self
            .plugins_dir
            .join("cache")
            .join(&meta.publisher)
            .join(name);

        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir).map_err(|e| format!("删除插件目录失败: {e}"))?;
        }

        self.save_registry()?;
        Ok(())
    }

    /// 列出已安装插件
    pub fn list(&self) -> Vec<&PluginMeta> {
        self.registry.plugins.values().collect()
    }

    /// 获取插件的安装目录
    pub fn plugin_dir(&self, name: &str) -> Option<PathBuf> {
        let meta = self.registry.plugins.get(name)?;
        Some(
            self.plugins_dir
                .join("cache")
                .join(&meta.publisher)
                .join(name)
                .join(&meta.version),
        )
    }

    /// 获取所有插件的 skills/ 目录路径
    pub fn skill_roots(&self) -> Vec<PathBuf> {
        self.registry
            .plugins
            .values()
            .filter_map(|meta| {
                let dir = self
                    .plugins_dir
                    .join("cache")
                    .join(&meta.publisher)
                    .join(&meta.name)
                    .join(&meta.version)
                    .join("skills");
                if dir.exists() {
                    Some(dir)
                } else {
                    None
                }
            })
            .collect()
    }

    /// 获取所有插件的 MCP 配置文件路径
    pub fn mcp_configs(&self) -> Vec<PathBuf> {
        self.registry
            .plugins
            .values()
            .filter_map(|meta| {
                let plugin_dir = self
                    .plugins_dir
                    .join("cache")
                    .join(&meta.publisher)
                    .join(&meta.name)
                    .join(&meta.version);
                let manifest_path = plugin_dir.join("plugin.json");
                let content = std::fs::read_to_string(&manifest_path).ok()?;
                let manifest: PluginManifest = serde_json::from_str(&content).ok()?;
                manifest
                    .mcp_servers
                    .map(|rel| plugin_dir.join(rel.trim_start_matches("./")))
            })
            .collect()
    }

    fn save_registry(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.plugins_dir).map_err(|e| format!("创建插件目录失败: {e}"))?;
        let json = serde_json::to_string_pretty(&self.registry)
            .map_err(|e| format!("序列化注册表失败: {e}"))?;
        std::fs::write(self.plugins_dir.join("registry.json"), json)
            .map_err(|e| format!("写入 registry.json 失败: {e}"))?;
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn now_timestamp() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", dur.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_plugin_json(name: &str, version: &str) -> String {
        format!(
            r#"{{"name": "{name}", "version": "{version}", "description": "Test plugin", "skills": "./skills/"}}"#
        )
    }

    fn make_skill_md(name: &str, desc: &str) -> String {
        format!("---\nname: {name}\ndescription: \"{desc}\"\n---\n# {name}")
    }

    #[test]
    fn load_empty_plugins_dir() {
        let dir = TempDir::new().unwrap();
        let mgr = PluginManager::load(dir.path()).unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn load_with_no_registry_file() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("cache")).unwrap();
        let mgr = PluginManager::load(dir.path()).unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn install_and_list_plugin() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            source_dir.path().join("plugin.json"),
            make_plugin_json("test-plugin", "1.0.0"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            make_skill_md("tdd", "TDD skill"),
        )
        .unwrap();

        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        let name = mgr.install(source_dir.path(), "test-publisher").unwrap();
        assert_eq!(name, "test-plugin");

        let plugins = mgr.list();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-plugin");
        assert_eq!(plugins[0].version, "1.0.0");
    }

    #[test]
    fn skill_roots_after_install() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            source_dir.path().join("plugin.json"),
            make_plugin_json("test-plugin", "1.0.0"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            make_skill_md("tdd", "TDD skill"),
        )
        .unwrap();

        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        mgr.install(source_dir.path(), "test-publisher").unwrap();

        let roots = mgr.skill_roots();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].to_string_lossy().contains("skills"));
    }

    #[test]
    fn uninstall_removes_plugin() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            source_dir.path().join("plugin.json"),
            make_plugin_json("test-plugin", "1.0.0"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            make_skill_md("tdd", "TDD skill"),
        )
        .unwrap();

        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        mgr.install(source_dir.path(), "test-publisher").unwrap();
        assert_eq!(mgr.list().len(), 1);

        mgr.uninstall("test-plugin").unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn install_missing_plugin_json_fails() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();
        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        assert!(mgr.install(source_dir.path(), "pub").is_err());
    }

    #[test]
    fn reload_persistent_registry() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(
            source_dir.path().join("plugin.json"),
            make_plugin_json("test-plugin", "1.0.0"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("SKILL.md"),
            make_skill_md("tdd", "TDD skill"),
        )
        .unwrap();

        {
            let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
            mgr.install(source_dir.path(), "test-publisher").unwrap();
        }

        let mgr2 = PluginManager::load(plugins_dir.path()).unwrap();
        assert_eq!(mgr2.list().len(), 1);
    }
}
