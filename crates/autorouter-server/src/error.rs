//! Server error types.

use thiserror::Error;

pub type ServerResult<T> = std::result::Result<T, ServerError>;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid request: {0}")]
    BadRequest(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl axum::response::IntoResponse for ServerError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, code) = match &self {
            ServerError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ServerError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ServerError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ServerError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            ServerError::Io(_) | ServerError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };
        let body = axum::Json(serde_json::json!({
            "error": {
                "code": code,
                "message": self.to_string(),
            }
        }));
        (status, body).into_response()
    }
}
