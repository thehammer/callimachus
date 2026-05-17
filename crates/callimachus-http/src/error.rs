use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use callimachus_core::types::{ToolError, ToolResult};

/// Unified HTTP error type: maps CalError / ToolError variants to status codes.
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}

impl From<callimachus_core::error::CalError> for ApiError {
    fn from(e: callimachus_core::error::CalError) -> Self {
        use callimachus_core::error::CalError;
        match &e {
            CalError::CorpusNotFound(_) | CalError::NotFound(_) => {
                ApiError::new(StatusCode::NOT_FOUND, e.to_string())
            }
            _ => ApiError::internal(e.to_string()),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::internal(e.to_string())
    }
}

/// Convert a `ToolResult<T>` into an Axum response.
///
/// Maps ToolError variants:
///   not_found   → 404
///   ambiguous   → 422
///   invalid_input → 400
///   error       → 500
pub fn tool_result_to_response<T: serde::Serialize>(
    result: ToolResult<T>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match result {
        ToolResult::Ok(success) => Ok(Json(serde_json::to_value(success).map_err(ApiError::from)?)),
        ToolResult::Err(err) => {
            let (status, message) = match &err.error {
                ToolError::NotFound { suggestions } => {
                    let msg = match suggestions {
                        Some(s) if !s.is_empty() => {
                            format!("not found. Suggestions: {}", s.join(", "))
                        }
                        _ => "not found".to_string(),
                    };
                    (StatusCode::NOT_FOUND, msg)
                }
                ToolError::Ambiguous { candidates } => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("ambiguous; candidates: {}", candidates.join(", ")),
                ),
                ToolError::InvalidInput { message } => (StatusCode::BAD_REQUEST, message.clone()),
                ToolError::Error { message, .. } => {
                    (StatusCode::INTERNAL_SERVER_ERROR, message.clone())
                }
            };
            Err(ApiError::new(status, message))
        }
    }
}
