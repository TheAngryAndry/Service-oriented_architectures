use axum::{
    Router,
    http::{Method, StatusCode, header},
    response::IntoResponse,
};
use std::env;

fn app() -> Router {
    Router::new().fallback(handle_request)
}

async fn handle_request(method: Method, uri: axum::http::Uri) -> impl IntoResponse {
    let (status, body) = match (method, uri.path()) {
        (Method::GET, "/health") => (StatusCode::OK, "{\"status\":\"ok\"}\n"),
        (Method::GET, _) => (StatusCode::NOT_FOUND, "{\"error\":\"not_found\"}\n"),
        _ => (
            StatusCode::NOT_IMPLEMENTED,
            "{\"error\":\"not_implemented\"}\n",
        ),
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
        body,
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = env::var("PORT").unwrap_or_else(|_| "8080".into()).parse()?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("Orders Service listening on {}", listener.local_addr()?);
    axum::serve(listener, app()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_json_with_and_without_query() {
        for path in ["/health", "/health?probe=docker"] {
            let response = app()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "application/json; charset=utf-8"
            );
            assert_eq!(response.headers()[header::CONTENT_LENGTH], "16");
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                "{\"status\":\"ok\"}\n"
            );
        }
    }

    #[tokio::test]
    async fn business_and_unknown_routes_are_not_implemented() {
        for path in ["/", "/orders", "/payments", "/health/", "/healthz"] {
            let response = app()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                "{\"error\":\"not_found\"}\n"
            );
        }
    }

    #[tokio::test]
    async fn unsupported_methods_do_not_create_business_objects() {
        for method in [Method::POST, Method::PUT, Method::DELETE] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/orders")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        }
    }
}
