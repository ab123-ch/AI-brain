use crate::ports::NovelPortError;

pub type Result<T> = std::result::Result<T, NovelBrainError>;

#[derive(Debug, thiserror::Error)]
pub enum NovelBrainError {
    #[error("小说脑命令队列已满，请稍后重试")]
    Backpressure,
    #[error("小说脑服务已停止")]
    Unavailable,
    #[error("小说脑任务合同无效: {0}")]
    InvalidRequest(String),
    #[error("小说脑状态转换无效: {0}")]
    InvalidTransition(String),
    #[error("小说脑任务不存在: {0}")]
    TaskNotFound(String),
    #[error("小说项目 {0} 已有活动写作任务")]
    ProjectBusy(String),
    #[error("小说 Canon revision 已过期: expected={expected}, actual={actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("小说脑模型调用失败: {0}")]
    Model(String),
    #[error("小说脑返回协议无效: {0}")]
    InvalidModelOutput(String),
    #[error("小说脑端口失败: {0}")]
    Port(#[from] NovelPortError),
    #[error("小说脑状态序列化失败: {0}")]
    Serialization(#[from] serde_json::Error),
}
