use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{Duration as Days, NaiveDate, Utc};
use cinema_analytics::{ch, env};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Serialize, Deserialize)]
struct Metric {
    metric: String,
    dimension: String,
    value: f64,
}
struct App {
    db: sqlx::PgPool,
    lock: tokio::sync::Mutex<()>,
}
async fn calculate(a: &App, date: NaiveDate) -> anyhow::Result<()> {
    let _guard = a.lock.lock().await;
    let start = Instant::now();
    tracing::info!(%date,"aggregation started");
    let rows =
        ch(&include_str!("../../sql/metrics.sql").replace("{date}", &date.to_string())).await?;
    let metrics: Vec<Metric> = rows
        .lines()
        .filter(|s| !s.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let now = Utc::now();
    let mut lines = String::new();
    for m in &metrics {
        lines.push_str(&serde_json::json!({"date":date,"metric":m.metric,"dimension":m.dimension,"value":m.value,"computed_at":now.format("%Y-%m-%d %H:%M:%S%.3f").to_string()}).to_string());
        lines.push('\n');
    }
    if !lines.is_empty() {
        ch(&format!(
            "INSERT INTO daily_metrics FORMAT JSONEachRow\n{lines}"
        ))
        .await?;
    }
    for attempt in 0..3 {
        let result=async {
            let mut tx=a.db.begin().await?;
            for m in &metrics {
                sqlx::query("INSERT INTO daily_metrics(date,metric,dimension,value,computed_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(date,metric,dimension) DO UPDATE SET value=excluded.value,computed_at=excluded.computed_at")
                    .bind(date).bind(&m.metric).bind(&m.dimension).bind(m.value).bind(now).execute(&mut *tx).await?;
            }
            tx.commit().await
        }.await;
        match result {
            Ok(()) => break,
            Err(e) => {
                tracing::error!(%e,attempt,"PostgreSQL retry");
                if attempt == 2 {
                    return Err(e.into());
                }
                tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
            }
        }
    }
    let processed = metrics
        .iter()
        .find(|m| m.metric == "events_processed")
        .map(|m| m.value)
        .unwrap_or(0.0);
    tracing::info!(%date,processed,metrics=metrics.len(),elapsed_ms=start.elapsed().as_millis(),"aggregation completed");
    Ok(())
}
async fn export(a: &App, date: NaiveDate) -> anyhow::Result<()> {
    let rows=sqlx::query("SELECT metric,dimension,value,computed_at FROM daily_metrics WHERE date=$1 ORDER BY metric,dimension").bind(date).fetch_all(&a.db).await?;
    anyhow::ensure!(!rows.is_empty(), "no metrics for {date}; calculate first");
    let data:Vec<_>=rows.iter().map(|r|serde_json::json!({"date":date,"metric":r.get::<String,_>("metric"),"dimension":r.get::<String,_>("dimension"),"value":r.get::<f64,_>("value"),"computed_at":r.get::<chrono::DateTime<Utc>,_>("computed_at")})).collect();
    let path = format!("/tmp/aggregates-{}.json", uuid::Uuid::new_v4());
    let payload = serde_json::to_vec_pretty(&data)?;
    let hash_header = format!("x-amz-content-sha256:{:x}", Sha256::digest(&payload));
    tokio::fs::write(&path, payload).await?;
    let target = format!(
        "{}/movie-analytics/daily/{date}/aggregates.json",
        env("S3_ENDPOINT", "http://s3:9000")
    );
    let credentials = format!(
        "{}:{}",
        env("S3_ACCESS_KEY", "cinema"),
        env("S3_SECRET_KEY", "cinema-local-secret")
    );
    let result = tokio::process::Command::new("curl")
        .args([
            "--fail-with-body",
            "--silent",
            "--show-error",
            "--max-time",
            "60",
            "--aws-sigv4",
            "aws:amz:us-east-1:s3",
            "--user",
            &credentials,
            "--header",
            &hash_header,
            "--header",
            "Content-Type: application/json",
            "--upload-file",
            &path,
            &target,
        ])
        .output()
        .await;
    let _ = tokio::fs::remove_file(&path).await;
    let output = result?;
    anyhow::ensure!(
        output.status.success(),
        "S3 upload failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    tracing::info!(%date,"export completed");
    Ok(())
}
fn date(s: &str) -> Result<NaiveDate, (StatusCode, String)> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| (StatusCode::BAD_REQUEST, "expected YYYY-MM-DD".into()))
}
fn failed(e: anyhow::Error) -> (StatusCode, String) {
    tracing::error!(%e,"operation failed");
    (StatusCode::SERVICE_UNAVAILABLE, e.to_string())
}
async fn recalc(
    State(a): State<Arc<App>>,
    Path(s): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let d = date(&s)?;
    calculate(&a, d).await.map_err(failed)?;
    Ok(Json(serde_json::json!({"date":d,"status":"ok"})))
}
async fn download(
    State(a): State<Arc<App>>,
    Path(s): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let d = date(&s)?;
    export(&a, d).await.map_err(failed)?;
    Ok(Json(serde_json::json!({"date":d,"status":"ok"})))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cinema_analytics::logs();
    let db = sqlx::PgPool::connect(&env(
        "DATABASE_URL",
        "postgres://cinema:cinema-local@postgres/cinema",
    ))
    .await?;
    sqlx::raw_sql(include_str!("../../sql/postgres.sql"))
        .execute(&db)
        .await?;
    let a = Arc::new(App {
        db,
        lock: tokio::sync::Mutex::new(()),
    });
    let background = a.clone();
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(
            env("AGGREGATION_INTERVAL_SECONDS", "300")
                .parse::<u64>()
                .unwrap_or(300)
                .max(1),
        ));
        loop {
            timer.tick().await;
            // Recompute immature cohorts so D1..D7 become available; late data can be backfilled with HTTP.
            for age in 0..=7 {
                let d = Utc::now().date_naive() - Days::days(age);
                if let Err(e) = calculate(&background, d).await {
                    tracing::error!(%e,%d,"scheduled calculation failed; retry next cycle");
                }
            }
            let d = Utc::now().date_naive() - Days::days(1);
            if let Err(e) = export(&background, d).await {
                tracing::error!(%e,"export failed; retry next cycle");
            }
        }
    });
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/aggregate/{date}", post(recalc))
        .route("/export/{date}", post(download))
        .with_state(a);
    axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8080").await?, app).await?;
    Ok(())
}
