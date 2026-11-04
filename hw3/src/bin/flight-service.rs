use flight_booking::{
    cache::Cache,
    flight::{Flights, authenticate},
    pb::flight_service_server::FlightServiceServer,
    settings,
};
use sqlx::postgres::PgPoolOptions;
use tonic::transport::Server;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    settings::logging();
    let db = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(3))
        .connect(&settings::required("DATABASE_URL")?)
        .await?;
    sqlx::migrate!("./migrations/flight").run(&db).await?;
    let key = settings::required("GRPC_API_KEY")?;
    let nodes = settings::required("REDIS_CLUSTER_NODES")?
        .split(',')
        .map(str::to_owned)
        .collect();
    let cache = Cache::new(
        nodes,
        settings::number("CACHE_TTL_SECONDS", 300)?,
        settings::duration("CACHE_TIMEOUT_MS", 200)?,
    )?;
    let address = std::env::var("GRPC_ADDRESS")
        .unwrap_or_else(|_| "0.0.0.0:50051".into())
        .parse()?;
    tracing::info!(%address,"flight service listening");
    Server::builder()
        .add_service(FlightServiceServer::with_interceptor(
            Flights { db, cache },
            authenticate(key),
        ))
        .serve_with_shutdown(address, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
