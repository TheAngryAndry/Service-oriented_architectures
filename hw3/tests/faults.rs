//! Destructive only to this homework's containers. Run on the Docker host, separately.
#![cfg(feature = "fault-tests")]
use flight_booking::pb::{GetFlightRequest, flight_service_client::FlightServiceClient};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::{
    process::Command,
    time::{Duration, Instant},
};
use uuid::Uuid;

fn compose(args: &[&str]) -> String {
    let mut command = if std::env::var("DOCKER_SUDO").as_deref() == Ok("1") {
        let mut c = Command::new("sudo");
        c.args(["-n", "docker"]);
        c
    } else {
        Command::new("docker")
    };
    let output = command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("compose")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "docker compose {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
struct Restart(Vec<&'static str>);
impl Drop for Restart {
    fn drop(&mut self) {
        let mut args = vec!["start"];
        args.extend(self.0.iter().copied());
        compose(&args);
    }
}
fn base() -> String {
    std::env::var("BOOKING_URL").unwrap_or_else(|_| "http://127.0.0.1:8083".into())
}
fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap()
}
async fn wait_api(client: &Client) {
    for _ in 0..80 {
        if let Ok(r) = client
            .get(format!(
                "{}/flights/11111111-1111-4111-8111-111111111111",
                base()
            ))
            .send()
            .await
            && r.status() == StatusCode::OK
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("Flight API did not recover");
}
async fn cluster_ready() {
    for _ in 0..80 {
        let info = compose(&["exec", "-T", "redis-2", "redis-cli", "cluster", "info"]);
        let mut ready = info.contains("cluster_state:ok");
        for service in [
            "redis-1", "redis-2", "redis-3", "redis-4", "redis-5", "redis-6",
        ] {
            let replication = compose(&["exec", "-T", service, "redis-cli", "info", "replication"]);
            ready &= if replication.contains("role:slave") {
                replication.contains("master_link_status:up")
                    && replication.contains("master_sync_in_progress:0")
            } else {
                replication.contains("connected_slaves:1") && replication.contains("state=online")
            };
        }
        if ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("Redis Cluster and its replicas did not recover");
}
#[tokio::test]
#[ignore = "stops the homework Flight container"]
async fn flight_outage_opens_breaker_and_recovers() {
    let http = client();
    wait_api(&http).await;
    compose(&["stop", "-t", "2", "flight"]);
    let restart = Restart(vec!["flight"]);
    for _ in 0..5 {
        assert_eq!(
            http.get(format!(
                "{}/flights/11111111-1111-4111-8111-111111111111",
                base()
            ))
            .send()
            .await
            .unwrap()
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    let start = Instant::now();
    let response = http
        .get(format!(
            "{}/flights/11111111-1111-4111-8111-111111111111",
            base()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        response.json::<Value>().await.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("circuit")
    );
    assert!(start.elapsed() < Duration::from_millis(500));
    // Reads of Booking's own database remain available while Flight is down.
    assert_eq!(
        http.get(format!("{}/bookings?user_id={}", base(), Uuid::new_v4()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(restart);
    wait_api(&http).await;
    let logs = compose(&["logs", "--no-color", "--tail", "300", "booking"]);
    assert!(logs.contains("CLOSED") && logs.contains("OPEN") && logs.contains("HALF_OPEN"));
}
#[tokio::test]
#[ignore = "stops all six homework Redis nodes"]
async fn redis_outage_falls_back_to_database_without_stale_recovery() {
    let http = client();
    let flight = "11111111-1111-4111-8111-111111111111";
    wait_api(&http).await;
    let before: Value = http
        .get(format!("{}/flights/{flight}", base()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    compose(&[
        "stop", "-t", "2", "redis-1", "redis-2", "redis-3", "redis-4", "redis-5", "redis-6",
    ]);
    let restart = Restart(vec![
        "redis-1", "redis-2", "redis-3", "redis-4", "redis-5", "redis-6",
    ]);
    let id = Uuid::new_v4();
    let response=http.post(format!("{}/bookings",base())).header("Idempotency-Key",id.to_string()).json(&json!({
        "user_id":Uuid::new_v4(),"flight_id":flight,"passenger_name":"Redis Outage","passenger_email":"outage@example.com","seat_count":1
    })).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let during: Value = http
        .get(format!("{}/flights/{flight}", base()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        during["available_seats"].as_i64(),
        before["available_seats"].as_i64().map(|n| n - 1)
    );
    drop(restart);
    cluster_ready().await;
    let after: Value = http
        .get(format!("{}/flights/{flight}", base()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["available_seats"], during["available_seats"]);
    assert_eq!(
        http.post(format!("{}/bookings/{id}/cancel", base()))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}
#[tokio::test]
#[ignore = "stops one Redis master and waits for replica promotion"]
async fn redis_replica_promotes_and_cluster_client_follows_topology() {
    cluster_ready().await;
    let nodes = compose(&["exec", "-T", "redis-2", "redis-cli", "cluster", "nodes"]);
    let master_line = nodes
        .lines()
        .find(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() > 7
                && fields[2].split(',').any(|f| f == "master")
                && fields[7] == "connected"
        })
        .expect("a live master");
    let fields: Vec<_> = master_line.split_whitespace().collect();
    let old_master = fields[0].to_string();
    let last_octet = fields[1]
        .split(':')
        .next()
        .unwrap()
        .rsplit('.')
        .next()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let service = match last_octet {
        11 => "redis-1",
        12 => "redis-2",
        13 => "redis-3",
        14 => "redis-4",
        15 => "redis-5",
        16 => "redis-6",
        _ => panic!("unexpected cluster address"),
    };
    // Ensure the elected observer stays alive.
    let observer = if service == "redis-2" {
        "redis-3"
    } else {
        "redis-2"
    };
    let replicas_before = nodes
        .lines()
        .filter(|l| l.split_whitespace().nth(3) == Some(old_master.as_str()))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(!replicas_before.is_empty());
    // Keep a real ClusterConnection alive across the failure (inside Flight Service).
    let mut grpc = FlightServiceClient::connect(
        std::env::var("FLIGHT_GRPC_URL").unwrap_or_else(|_| "http://127.0.0.1:50053".into()),
    )
    .await
    .unwrap();
    let mut request = tonic::Request::new(GetFlightRequest {
        id: "11111111-1111-4111-8111-111111111111".into(),
    });
    request.metadata_mut().insert(
        "x-api-key",
        std::env::var("GRPC_API_KEY")
            .unwrap_or_else(|_| "homework-local-api-key".into())
            .parse()
            .unwrap(),
    );
    grpc.get_flight(request).await.unwrap();
    compose(&["stop", "-t", "2", service]);
    let restart = Restart(vec![service]);
    let mut promoted = false;
    for _ in 0..100 {
        let current = compose(&["exec", "-T", observer, "redis-cli", "cluster", "nodes"]);
        promoted = current.lines().any(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            f.len() > 7
                && replicas_before.iter().any(|id| id == f[0])
                && f[2].split(',').any(|v| v == "master")
        });
        if promoted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(promoted, "replica was not promoted");
    wait_api(&client()).await;
    let info = compose(&["exec", "-T", observer, "redis-cli", "cluster", "info"]);
    assert!(info.contains("cluster_state:ok"));
    drop(restart);
    cluster_ready().await;
}
