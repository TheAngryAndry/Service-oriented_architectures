use orders::{AppState, app};
use sqlx::postgres::PgPoolOptions;
use std::{env, sync::Arc};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().json().flatten_event(true).init();
    let secret = env::var("JWT_SECRET")?;
    if secret.len() < 32 {
        return Err("JWT_SECRET must contain at least 32 bytes".into());
    }
    let interval: i64 = env::var("ORDER_INTERVAL_MINUTES")
        .unwrap_or_else(|_| "1".into())
        .parse()?;
    if !(0..=1440).contains(&interval) {
        return Err("ORDER_INTERVAL_MINUTES must be 0..1440".into());
    }
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&env::var("DATABASE_URL")?)
        .await?;
    // Flyway is the only migration runner. Fail startup when schema is absent.
    sqlx::query("SELECT id FROM user_operations LIMIT 0")
        .execute(&pool)
        .await?;
    let port: u16 = env::var("PORT").unwrap_or_else(|_| "8080".into()).parse()?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(port, "Marketplace API started");
    axum::serve(
        listener,
        app(AppState {
            db: pool,
            jwt_secret: Arc::new(secret),
            order_interval_minutes: interval,
        }),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
