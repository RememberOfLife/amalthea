use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[derive(Debug)]
pub struct Error(pub StatusCode, pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn bad(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, message.into())
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, message.into())
    }
    pub fn missing() -> Self {
        Self(
            StatusCode::NOT_FOUND,
            "Not found or no longer available".into(),
        )
    }
    pub fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "Please sign in".into())
    }
    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(%error, "operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The operation could not be completed. Please try again.".into(),
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}
impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Self::internal(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e)
    }
}
impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Self {
        Self::internal(e)
    }
}
