use flight_booking::{
    booking::{AppState, recovery_worker, router},
    client::{ClientConfig, FlightClient},
    settings,
};
use sqlx::postgres::PgPoolOptions;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    settings::logging();
    let db = PgPoolOptions::new()
        .max_connections(30)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&settings::required("DATABASE_URL")?)
        .await?;
    sqlx::migrate!("./migrations/booking").run(&db).await?;
    let config = ClientConfig {
        timeout: settings::duration("GRPC_TIMEOUT_MS", 2000)?,
        retry_base: settings::duration("RETRY_BASE_MS", 100)?,
        failure_threshold: settings::number("CB_FAILURE_THRESHOLD", 5)?.try_into()?,
        open_duration: settings::duration("CB_OPEN_MS", 10000)?,
    };
    let flights = FlightClient::new(
        settings::required("FLIGHT_GRPC_URL")?,
        settings::required("GRPC_API_KEY")?,
        config,
    )?;
    let state = AppState { db, flights };
    tokio::spawn(recovery_worker(state.clone()));
    let address = std::env::var("HTTP_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    tracing::info!(%address,"booking service listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
