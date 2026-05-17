#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("request failed ({status}): {message}")]
    ApiError { status: u16, message: String },

    #[error("subprocess error: {0}")]
    Subprocess(String),

    #[error("timeout after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("no provider configured (set ANTHROPIC_API_KEY or install claude CLI)")]
    NoProvider,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, LlmError>;
