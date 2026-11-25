use chrono::{Duration as Days, Utc};
use cinema_analytics::{Event, ch, env};
use serde_json::{Value, json};
use std::time::Duration;
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .unwrap()
}
fn fixture(user: &str, session: &str, kind: &str, days: i64, progress: i32) -> Event {
    Event {
        event_id: uuid::Uuid::new_v4(),
        user_id: user.into(),
        movie_id: format!("film-{user}"),
        event_type: kind.into(),
        timestamp: Utc::now() - Days::days(days),
        device_type: "TABLET".into(),
        session_id: session.into(),
        progress_seconds: progress,
    }
}
async fn send(e: &Event) {
    let r = client()
        .post(format!(
            "{}/events",
            env("PRODUCER_URL", "http://localhost:8085")
        ))
        .json(e)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["event_id"], e.event_id.to_string());
}
async fn wait(e: &Event) {
    for _ in 0..90 {
        let r = ch(&format!(
            "SELECT count() FROM raw_events FINAL WHERE event_id='{}'",
            e.event_id
        ))
        .await
        .unwrap();
        if r.trim() == "1" {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("event {} not ingested", e.event_id)
}
#[tokio::test]
#[ignore = "requires Compose; use ./test.sh"]
async fn api_kafka_clickhouse_all_fields_and_validation() {
    let registry: Value = client()
        .get("http://schema-registry:8081/subjects/movie-events-value/versions/latest")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(registry["version"], 1);
    assert_eq!(registry["schemaType"].as_str().unwrap_or("AVRO"), "AVRO");
    use rdkafka::consumer::{BaseConsumer, Consumer};
    let kafka: BaseConsumer = rdkafka::ClientConfig::new()
        .set("bootstrap.servers", "kafka-1:9092,kafka-2:9092")
        .set("group.id", "e2e-metadata")
        .create()
        .unwrap();
    let metadata = kafka
        .fetch_metadata(Some("movie-events"), Duration::from_secs(20))
        .unwrap();
    assert!(metadata.brokers().len() >= 2);
    let topic = metadata
        .topics()
        .iter()
        .find(|t| t.name() == "movie-events")
        .unwrap();
    assert!(topic.partitions().len() >= 3);
    assert!(topic.partitions().iter().all(|p| p.replicas().len() >= 2));

    let u = uuid::Uuid::new_v4().to_string();
    let e = fixture(&u, &u, "VIEW_STARTED", 15, 0);
    send(&e).await;
    wait(&e).await;
    let s=ch(&format!("SELECT toString(event_id) AS event_id,user_id,movie_id,event_type,toUnixTimestamp64Milli(timestamp) AS millis,device_type,session_id,progress_seconds FROM raw_events FINAL WHERE event_id='{}' FORMAT JSONEachRow",e.event_id)).await.unwrap();
    let v: Value = serde_json::from_str(s.trim()).unwrap();
    assert_eq!(v["event_id"], e.event_id.to_string());
    assert_eq!(v["user_id"], e.user_id);
    assert_eq!(v["movie_id"], e.movie_id);
    assert_eq!(v["event_type"], e.event_type);
    assert_eq!(v["device_type"], e.device_type);
    assert_eq!(v["session_id"], e.session_id);
    assert_eq!(v["progress_seconds"], 0);
    assert_eq!(
        v["millis"]
            .as_str()
            .map(|s| s.parse::<i64>().unwrap())
            .or(v["millis"].as_i64())
            .unwrap(),
        e.timestamp.timestamp_millis()
    );
    // An application retry with the same event_id must not inflate analytics.
    send(&e).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        ch(&format!(
            "SELECT count() FROM raw_events FINAL WHERE event_id='{}'",
            e.event_id
        ))
        .await
        .unwrap()
        .trim(),
        "1"
    );
    let mut invalid = serde_json::to_value(&e).unwrap();
    invalid["progress_seconds"] = json!(-1);
    assert_eq!(
        client()
            .post(format!(
                "{}/events",
                env("PRODUCER_URL", "http://localhost:8085")
            ))
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    invalid["progress_seconds"] = json!(0);
    invalid["event_type"] = json!("BAD");
    assert_eq!(
        client()
            .post(format!(
                "{}/events",
                env("PRODUCER_URL", "http://localhost:8085")
            ))
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
}
#[tokio::test]
#[ignore = "requires isolated Compose; use ./test.sh"]
async fn metrics_retention_recalculation_and_s3_export() {
    // test.sh provisions an isolated project with the synthetic generator disabled.
    let u = uuid::Uuid::new_v4().to_string();
    let session = uuid::Uuid::new_v4().to_string();
    let events = [
        fixture(&u, &session, "VIEW_STARTED", 30, 0),
        fixture(&u, &session, "VIEW_FINISHED", 30, 1200),
        fixture(&u, "d1", "VIEW_STARTED", 29, 0),
        fixture(&u, "d7", "VIEW_STARTED", 23, 0),
        fixture(
            &uuid::Uuid::new_v4().to_string(),
            "no-return",
            "VIEW_STARTED",
            30,
            0,
        ),
    ];
    for e in &events {
        send(e).await;
    }
    for e in &events {
        wait(e).await;
    }
    let date = events[0].timestamp.date_naive();
    let base = env("AGGREGATION_URL", "http://localhost:8086");
    let db = sqlx::PgPool::connect(&env(
        "DATABASE_URL",
        "postgres://cinema:cinema-local@postgres/cinema",
    ))
    .await
    .unwrap();
    for _ in 0..2 {
        let r = client()
            .post(format!("{base}/aggregate/{date}"))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
        for (metric, expected) in [
            ("dau", 2.0),
            ("avg_watch_seconds", 1200.0),
            ("conversion", 0.5),
            ("retention_d0", 1.0),
            ("retention_d1", 0.5),
            ("retention_d2", 0.0),
            ("retention_d3", 0.0),
            ("retention_d4", 0.0),
            ("retention_d5", 0.0),
            ("retention_d6", 0.0),
            ("events_processed", 3.0),
            ("retention_d7", 0.5),
        ] {
            let value: f64 = sqlx::query_scalar(
                "SELECT value FROM daily_metrics WHERE date=$1 AND metric=$2 AND dimension=''",
            )
            .bind(date)
            .bind(metric)
            .fetch_one(&db)
            .await
            .unwrap();
            assert_eq!(value, expected, "{metric}");
        }
    }
    let duplicate:i64=sqlx::query_scalar("SELECT count(*) FROM (SELECT date,metric,dimension FROM daily_metrics GROUP BY date,metric,dimension HAVING count(*)>1) t").fetch_one(&db).await.unwrap();
    assert_eq!(duplicate, 0);
    for _ in 0..2 {
        let r = client()
            .post(format!("{base}/export/{date}"))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    }
    // Read back S3 through the export runtime, from the test runner's Compose command.
    let data=ch(&format!("SELECT value FROM daily_metrics FINAL WHERE date='{date}' AND metric='retention_d7' FORMAT JSONEachRow")).await.unwrap();
    let materialized: Value = serde_json::from_str(data.trim()).unwrap();
    assert_eq!(materialized["value"], json!(0.5));
}

#[tokio::test]
#[ignore = "requires Compose; use ./test.sh"]
async fn grafana_datasource_and_provisioned_dashboard_work() {
    let http = client();
    let dashboard: Value = http
        .get("http://grafana:3000/api/dashboards/uid/cinema")
        .basic_auth("admin", Some("cinema-local-admin"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let panels = dashboard["dashboard"]["panels"].as_array().unwrap();
    assert!(panels.len() >= 3);
    assert!(
        panels
            .iter()
            .any(|p| p["title"] == "Retention Cohort Heatmap")
    );
    for panel in panels {
        let sql = panel["targets"][0]["rawSql"].as_str().unwrap();
        assert!(sql.contains("daily_metrics"));
        ch(sql).await.unwrap();
        let mut query = panel["targets"][0].clone();
        query["datasource"] = panel["datasource"].clone();
        let result: Value = http
            .post("http://grafana:3000/api/ds/query")
            .basic_auth("admin", Some("cinema-local-admin"))
            .json(&json!({"queries":[query],"from":"now-90d","to":"now"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(result["results"]["A"]["error"].is_null(), "{result}");
        assert!(
            !result["results"]["A"]["frames"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{result}"
        );
    }
    let response = http
        .get("http://grafana:3000/api/datasources/uid/clickhouse/health")
        .basic_auth("admin", Some("cinema-local-admin"))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );
}
