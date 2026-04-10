use std::path::PathBuf;

/// 记忆脑统一错误
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("存储路径不存在: {0}")]
    PathNotFound(PathBuf),

    #[error("记忆条目不存在: {0}")]
    EntryNotFound(String),

    #[error("总线错误: {0}")]
    Bus(#[from] brain_bus::BusError),

    #[error("巩固失败: {0}")]
    ConsolidationFailed(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;
