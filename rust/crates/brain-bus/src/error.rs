use thiserror::Error;

#[derive(Debug, Error)]
pub enum BusError {
    #[error("channel closed")]
    ChannelClosed,

    #[error("channel full, cannot send")]
    ChannelFull,

    #[error("hop count exceeded ({hop_count} > 3), possible routing loop")]
    HopCountExceeded { hop_count: u32 },

    #[error("no active subscription for brain: {0}")]
    NoSubscription(String),

    #[error("broadcast send failed: no active receivers")]
    NoReceivers,
}
