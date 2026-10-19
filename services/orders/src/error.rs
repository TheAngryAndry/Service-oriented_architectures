use crate::{contract, generated::ErrorResponse};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct ApiError {
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}
pub type Result<T> = std::result::Result<T, ApiError>;
impl ApiError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }
    pub fn details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn validation(field: &str, message: &str) -> Self {
        Self::new("VALIDATION_ERROR", "Некорректные входные данные")
            .details(json!({"violations": [{"field": field, "message": message}]}))
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = contract::spec()["x-error-statuses"][self.code]
            .as_u64()
            .unwrap_or(500) as u16;
        (
            StatusCode::from_u16(status).unwrap(),
            Json(ErrorResponse {
                error_code: self.code.into(),
                message: self.message,
                details: self.details,
            }),
        )
            .into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        // SQL errors are never returned to clients (may contain private data).
        tracing::error!(error = %e, "Database operation failed");
        Self::new("INTERNAL_ERROR", "Внутренняя ошибка сервера")
    }
}
impl From<serde_json::Error> for ApiError {
    fn from(_: serde_json::Error) -> Self {
        Self::validation("body", "JSON не соответствует контракту")
    }
}
