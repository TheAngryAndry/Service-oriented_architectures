pub mod api;
pub mod auth;
pub mod catalog;
pub mod contract;
pub mod error;
pub mod orders;
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/api.rs"));
}

use axum::{Router, middleware};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub jwt_secret: Arc<String>,
    pub order_interval_minutes: i64,
}

pub fn app(state: AppState) -> Router {
    generated::routes()
        .route(
            "/openapi.json",
            axum::routing::get(|| async { axum::Json(contract::spec().clone()) }),
        )
        .fallback(|| async {
            error::ApiError::new("NOT_FOUND", "Маршрут не найден")
        })
        .method_not_allowed_fallback(|| async {
            error::ApiError::new("METHOD_NOT_ALLOWED", "Метод не поддерживается")
        })
        .layer(middleware::from_fn_with_state(
            state.clone(),
            api::log_request,
        ))
        .with_state(state)
}
