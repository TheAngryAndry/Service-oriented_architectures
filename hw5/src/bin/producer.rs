use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use cinema_analytics::{Event, env};
use rdkafka::{
    ClientConfig,
    producer::{FutureProducer, FutureRecord},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
struct App {
    producer: FutureProducer,
    schema: apache_avro::Schema,
    id: u32,
}
async fn publish(a: &App, e: &Event) -> anyhow::Result<()> {
    let mut data = vec![0];
    data.extend(a.id.to_be_bytes());
    data.extend(apache_avro::to_avro_datum(&a.schema, e.avro()?)?);
    // librdkafka retries with exponential backoff, maintaining idempotent sequence numbers.
    a.producer
        .send(
            FutureRecord::to("movie-events")
                .key(&e.user_id)
                .payload(&data),
            Duration::from_secs(10),
        )
        .await
        .map_err(|(e, _)| anyhow::anyhow!(e))?;
    tracing::info!(event_id=%e.event_id,event_type=%e.event_type,timestamp=%e.timestamp,"published");
    Ok(())
}
async fn event(
    State(a): State<Arc<App>>,
    Json(e): Json<Event>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, String)> {
    e.validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    publish(&a, &e).await.map_err(|e| {
        tracing::error!(%e,"publish failed");
        (StatusCode::SERVICE_UNAVAILABLE, "Kafka unavailable".into())
    })?;
    Ok((StatusCode::CREATED, Json(json!({"event_id":e.event_id}))))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cinema_analytics::logs();
    let schema_text = include_str!("../../schemas/movie-event-v1.avsc");
    let client = reqwest::Client::new();
    let registry = env("SCHEMA_REGISTRY_URL", "http://schema-registry:8081");
    client
        .put(format!("{registry}/config/movie-events-value"))
        .json(&json!({"compatibility":"BACKWARD"}))
        .send()
        .await?
        .error_for_status()?;
    let response: Value = client
        .post(format!("{registry}/subjects/movie-events-value/versions"))
        .json(&json!({"schemaType":"AVRO","schema":schema_text}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let a = Arc::new(App {
        schema: apache_avro::Schema::parse_str(schema_text)?,
        id: response["id"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("missing schema id"))? as u32,
        producer: ClientConfig::new()
            .set(
                "bootstrap.servers",
                env("KAFKA_BROKERS", "kafka-1:9092,kafka-2:9092"),
            )
            .set("enable.idempotence", "true")
            .set("acks", "all")
            .set("message.timeout.ms", "30000")
            .set("retry.backoff.ms", "100")
            .set("retry.backoff.max.ms", "5000")
            .create()?,
    });
    if env("GENERATE_EVENTS", "true") == "true" {
        let a = a.clone();
        tokio::spawn(async move {
            let mut n = 0u64;
            loop {
                n += 1;
                let user = format!("demo-{}", n % 100);
                let session = uuid::Uuid::new_v4().to_string();
                for (kind, progress) in [
                    ("SEARCHED", 0),
                    ("VIEW_STARTED", 0),
                    ("VIEW_PAUSED", 300),
                    ("VIEW_RESUMED", 300),
                    ("VIEW_FINISHED", 1800),
                    ("LIKED", 1800),
                ] {
                    let e = Event {
                        event_id: uuid::Uuid::new_v4(),
                        user_id: user.clone(),
                        movie_id: format!("movie-{}", n % 10),
                        event_type: kind.into(),
                        timestamp: chrono::Utc::now(),
                        device_type: "TV".into(),
                        session_id: session.clone(),
                        progress_seconds: progress,
                    };
                    if let Err(error) = publish(&a, &e).await {
                        tracing::error!(%error,"generator sequence stopped");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        });
    }
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/events", post(event))
        .with_state(a);
    axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8080").await?, app).await?;
    Ok(())
}
