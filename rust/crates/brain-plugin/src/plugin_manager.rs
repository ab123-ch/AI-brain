use std::path::PathBuf;

/// 插件元数据
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub installed_at: String,
    pub source: String,
}

/// 插件安装来源
#[derive(Debug, Clone)]
pub enum PluginSource {
    Local { path: PathBuf },
}

/// 插件管理器
pub struct PluginManager {
    plugins_dir: PathBuf,
}
