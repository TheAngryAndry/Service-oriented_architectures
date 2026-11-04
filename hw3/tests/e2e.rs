//! Real REST -> gRPC -> PostgreSQL + Redis checks. Run with the Compose test profile.
use flight_booking::pb::{flight_service_client::FlightServiceClient, *};
use prost::Message;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use tonic::{Code, Request, transport::Channel};
use uuid::Uuid;

struct Env {
    http: Client,
    base: String,
    grpc: FlightServiceClient<Channel>,
    flights: PgPool,
    bookings: PgPool,
    key: String,
}
impl Env {
    async fn new() -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
            base: std::env::var("BOOKING_URL").unwrap(),
            grpc: FlightServiceClient::connect(std::env::var("FLIGHT_GRPC_URL").unwrap())
                .await
                .unwrap(),
            flights: PgPool::connect(&std::env::var("FLIGHT_DATABASE_URL").unwrap())
                .await
                .unwrap(),
            bookings: PgPool::connect(&std::env::var("BOOKING_DATABASE_URL").unwrap())
                .await
                .unwrap(),
            key: std::env::var("GRPC_API_KEY").unwrap(),
        }
    }
    fn request<T>(&self, value: T) -> Request<T> {
        let mut r = Request::new(value);
        r.metadata_mut()
            .insert("x-api-key", self.key.parse().unwrap());
        r
    }
    async fn fixture(&self, seats: i32) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO flights(id,flight_number,airline_code,origin,destination,departure_time,arrival_time,total_seats,available_seats,price_minor,currency,status) VALUES($1,$2,'SU','VKO','LED','2050-01-01T10:00:00Z','2050-01-01T12:00:00Z',$3,$3,12345,'RUB','SCHEDULED')")
            .bind(id).bind(format!("TEST{}",id.simple())).bind(seats).execute(&self.flights).await.unwrap();
        sqlx::query("UPDATE cache_revision SET revision=revision+1")
            .execute(&self.flights)
            .await
            .unwrap();
        id
    }
    async fn flight(&self, id: Uuid) -> Flight {
        self.grpc
            .clone()
            .get_flight(self.request(GetFlightRequest { id: id.to_string() }))
            .await
            .unwrap()
            .into_inner()
    }
    fn body(&self, flight: Uuid, seats: i32, user: Uuid) -> Value {
        json!({"user_id":user,"flight_id":flight,"seat_count":seats,"passenger_name":"Test Passenger","passenger_email":"test@example.com"})
    }
    async fn create(&self, flight: Uuid, seats: i32, user: Uuid, key: Uuid) -> reqwest::Response {
        self.http
            .post(format!("{}/bookings", self.base))
            .header("Idempotency-Key", key.to_string())
            .json(&self.body(flight, seats, user))
            .send()
            .await
            .unwrap()
    }
    async fn redis(&self) -> redis::cluster_async::ClusterConnection {
        redis::cluster::ClusterClient::new(
            std::env::var("REDIS_CLUSTER_NODES")
                .unwrap()
                .split(',')
                .collect::<Vec<_>>(),
        )
        .unwrap()
        .get_async_connection()
        .await
        .unwrap()
    }
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn rest_booking_lifecycle_price_snapshot_and_idempotency() {
    let e = Env::new().await;
    let flight = e.fixture(10).await;
    let user = Uuid::new_v4();
    let id = Uuid::new_v4();
    let response = e.create(flight, 2, user, id).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let b: Value = response.json().await.unwrap();
    assert_eq!(b["total_price"], 24690);
    assert_eq!(b["status"], "CONFIRMED");
    assert_eq!(e.flight(flight).await.available_seats, 8);
    let duplicate = e.create(flight, 2, user, id).await;
    assert_eq!(duplicate.status(), StatusCode::OK);
    assert_eq!(duplicate.json::<Value>().await.unwrap()["id"], b["id"]);
    assert_eq!(
        e.create(flight, 3, user, id).await.status(),
        StatusCode::CONFLICT
    );
    e.grpc
        .clone()
        .update_flight(e.request(UpdateFlightRequest {
            id: flight.to_string(),
            price_minor: Some(99999),
            total_seats: None,
            status: None,
        }))
        .await
        .unwrap();
    let fetched: Value = e
        .http
        .get(format!("{}/bookings/{id}", e.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched["total_price"], 24690);
    let list: Vec<Value> = e
        .http
        .get(format!("{}/bookings?user_id={user}", e.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    let other: Vec<Value> = e
        .http
        .get(format!("{}/bookings?user_id={}", e.base, Uuid::new_v4()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(other.is_empty());
    for _ in 0..2 {
        let r = e
            .http
            .post(format!("{}/bookings/{id}/cancel", e.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.json::<Value>().await.unwrap()["status"], "CANCELLED");
    }
    assert_eq!(e.flight(flight).await.available_seats, 10);
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn validation_and_failed_reservation_do_not_create_bookings() {
    let e = Env::new().await;
    let f = e.fixture(1).await;
    let user = Uuid::new_v4();
    for (seats, status) in [
        (0, StatusCode::BAD_REQUEST),
        (-1, StatusCode::BAD_REQUEST),
        (2, StatusCode::CONFLICT),
    ] {
        assert_eq!(
            e.create(f, seats, user, Uuid::new_v4()).await.status(),
            status
        );
    }
    assert_eq!(e.flight(f).await.available_seats, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bookings WHERE user_id=$1")
        .bind(user)
        .fetch_one(&e.bookings)
        .await
        .unwrap();
    assert_eq!(count, 0);
    for path in [
        format!("/flights/{}", Uuid::new_v4()),
        format!("/bookings/{}", Uuid::new_v4()),
    ] {
        assert_eq!(
            e.http
                .get(format!("{}{path}", e.base))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    for path in [
        "/flights/not-a-uuid",
        "/flights?origin=SVO",
        "/flights?origin=svo&destination=LED",
        "/flights?origin=SVO&destination=LED&date=bad",
        "/bookings",
    ] {
        assert_eq!(
            e.http
                .get(format!("{}{path}", e.base))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    let mut body = e.body(f, 1, user);
    body["passenger_email"] = json!("invalid");
    assert_eq!(
        e.http
            .post(format!("{}/bookings", e.base))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn twenty_clients_compete_for_last_seat() {
    let e = Env::new().await;
    let flight = e.fixture(1).await;
    let user = Uuid::new_v4();
    let responses =
        futures::future::join_all((0..20).map(|_| e.create(flight, 1, user, Uuid::new_v4()))).await;
    let statuses = responses.iter().map(|r| r.status()).collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|&&s| s == StatusCode::CREATED)
            .count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|&&s| s == StatusCode::CONFLICT)
            .count(),
        19,
        "{statuses:?}"
    );
    let available: i32 = sqlx::query_scalar("SELECT available_seats FROM flights WHERE id=$1")
        .bind(flight)
        .fetch_one(&e.flights)
        .await
        .unwrap();
    assert_eq!(available, 0);
    let reserved: i64 = sqlx::query_scalar(
        "SELECT sum(seat_count) FROM seat_reservations WHERE flight_id=$1 AND status='ACTIVE'",
    )
    .bind(flight)
    .fetch_one(&e.flights)
    .await
    .unwrap();
    assert_eq!(reserved, 1);
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn grpc_authentication_protects_every_method() {
    let e = Env::new().await;
    for key in [None, Some("incorrect")] {
        let request = |body| {
            let mut r = Request::new(body);
            if let Some(key) = key {
                r.metadata_mut().insert("x-api-key", key.parse().unwrap());
            }
            r
        };
        // Explicit types keep every RPC covered, including the mutation endpoint.
        let mut c = e.grpc.clone();
        assert_eq!(
            c.get_flight(request(GetFlightRequest {
                id: Uuid::new_v4().to_string()
            }))
            .await
            .unwrap_err()
            .code(),
            Code::Unauthenticated
        );
        let mut r = Request::new(SearchFlightsRequest {
            origin: "SVO".into(),
            destination: "LED".into(),
            date: None,
        });
        if let Some(key) = key {
            r.metadata_mut().insert("x-api-key", key.parse().unwrap());
        }
        assert_eq!(
            c.search_flights(r).await.unwrap_err().code(),
            Code::Unauthenticated
        );
        let mut r = Request::new(ReserveSeatsRequest {
            flight_id: Uuid::new_v4().to_string(),
            booking_id: Uuid::new_v4().to_string(),
            seat_count: 1,
        });
        if let Some(key) = key {
            r.metadata_mut().insert("x-api-key", key.parse().unwrap());
        }
        assert_eq!(
            c.reserve_seats(r).await.unwrap_err().code(),
            Code::Unauthenticated
        );
        let mut r = Request::new(ReleaseReservationRequest {
            booking_id: Uuid::new_v4().to_string(),
        });
        if let Some(key) = key {
            r.metadata_mut().insert("x-api-key", key.parse().unwrap());
        }
        assert_eq!(
            c.release_reservation(r).await.unwrap_err().code(),
            Code::Unauthenticated
        );
        let mut r = Request::new(UpdateFlightRequest {
            id: Uuid::new_v4().to_string(),
            price_minor: Some(1),
            total_seats: None,
            status: None,
        });
        if let Some(key) = key {
            r.metadata_mut().insert("x-api-key", key.parse().unwrap());
        }
        assert_eq!(
            c.update_flight(r).await.unwrap_err().code(),
            Code::Unauthenticated
        );
    }
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn grpc_reserve_is_idempotent_and_release_fences_late_requests() {
    let e = Env::new().await;
    let f = e.fixture(5).await;
    let id = Uuid::new_v4();
    let reserve = ReserveSeatsRequest {
        flight_id: f.to_string(),
        booking_id: id.to_string(),
        seat_count: 2,
    };
    let results = futures::future::join_all((0..10).map(|_| {
        let mut c = e.grpc.clone();
        let r = e.request(reserve.clone());
        async move { c.reserve_seats(r).await.unwrap().into_inner() }
    }))
    .await;
    assert!(results.iter().all(|r| r.id == results[0].id));
    assert_eq!(e.flight(f).await.available_seats, 3);
    let mut conflict = reserve.clone();
    conflict.seat_count = 1;
    assert_eq!(
        e.grpc
            .clone()
            .reserve_seats(e.request(conflict))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    for _ in 0..2 {
        e.grpc
            .clone()
            .release_reservation(e.request(ReleaseReservationRequest {
                booking_id: id.to_string(),
            }))
            .await
            .unwrap();
    }
    assert_eq!(e.flight(f).await.available_seats, 5);
    assert_eq!(
        e.grpc
            .clone()
            .reserve_seats(e.request(reserve))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    let late = Uuid::new_v4();
    e.grpc
        .clone()
        .release_reservation(e.request(ReleaseReservationRequest {
            booking_id: late.to_string(),
        }))
        .await
        .unwrap();
    assert_eq!(
        e.grpc
            .clone()
            .reserve_seats(e.request(ReserveSeatsRequest {
                flight_id: f.to_string(),
                booking_id: late.to_string(),
                seat_count: 1
            }))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn cache_aside_ttl_and_invalidation_for_search_get_and_updates() {
    let e = Env::new().await;
    let f = e.fixture(5).await;
    let request = SearchFlightsRequest {
        origin: "VKO".into(),
        destination: "LED".into(),
        date: Some("2050-01-01".into()),
    };
    let before = e
        .grpc
        .clone()
        .search_flights(e.request(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert!(before.flights.iter().any(|v| v.id == f.to_string()));
    e.flight(f).await;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM cache_revision")
        .fetch_one(&e.flights)
        .await
        .unwrap();
    let key = format!("flight:{f}:v{revision}");
    let mut redis = e.redis().await;
    let ttl: i64 = redis::cmd("TTL")
        .arg(&key)
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!((1..=600).contains(&ttl), "{ttl}");
    let search_key = format!("search:VKO:LED:2050-01-01:v{revision}");
    let search_ttl: i64 = redis::cmd("TTL")
        .arg(search_key)
        .query_async(&mut redis)
        .await
        .unwrap();
    assert!((1..=600).contains(&search_ttl));
    // Change only the cached payload to prove the second GetFlight is a cache hit.
    let mut cached = e.flight(f).await;
    cached.airline_name = "CACHE_HIT_PROBE".into();
    let _: () = redis::cmd("SET")
        .arg(&key)
        .arg(cached.encode_to_vec())
        .arg("EX")
        .arg(300)
        .query_async(&mut redis)
        .await
        .unwrap();
    assert_eq!(e.flight(f).await.airline_name, "CACHE_HIT_PROBE");
    // Reserve invalidates the old namespace even if a reader repopulates an old key.
    let booking = Uuid::new_v4();
    e.grpc
        .clone()
        .reserve_seats(e.request(ReserveSeatsRequest {
            flight_id: f.to_string(),
            booking_id: booking.to_string(),
            seat_count: 2,
        }))
        .await
        .unwrap();
    let fresh = e.flight(f).await;
    assert_eq!(fresh.available_seats, 3);
    assert_ne!(fresh.airline_name, "CACHE_HIT_PROBE");
    let result = e
        .grpc
        .clone()
        .search_flights(e.request(request.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        result
            .flights
            .iter()
            .find(|v| v.id == f.to_string())
            .unwrap()
            .available_seats,
        3
    );
    e.grpc
        .clone()
        .release_reservation(e.request(ReleaseReservationRequest {
            booking_id: booking.to_string(),
        }))
        .await
        .unwrap();
    assert_eq!(e.flight(f).await.available_seats, 5);
    e.grpc
        .clone()
        .update_flight(e.request(UpdateFlightRequest {
            id: f.to_string(),
            price_minor: Some(99000),
            status: Some(FlightStatus::Cancelled as i32),
            total_seats: None,
        }))
        .await
        .unwrap();
    assert_eq!(e.flight(f).await.price_minor, 99000);
    let result = e
        .grpc
        .clone()
        .search_flights(e.request(request))
        .await
        .unwrap()
        .into_inner();
    assert!(!result.flights.iter().any(|v| v.id == f.to_string()));
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn database_constraints_and_non_scheduled_flights() {
    let e = Env::new().await;
    let f = e.fixture(2).await;
    for query in [
        "UPDATE flights SET available_seats=-1 WHERE id=$1",
        "UPDATE flights SET total_seats=0 WHERE id=$1",
        "UPDATE flights SET price_minor=0 WHERE id=$1",
        "UPDATE flights SET available_seats=3 WHERE id=$1",
    ] {
        assert!(
            sqlx::query(query)
                .bind(f)
                .execute(&e.flights)
                .await
                .is_err()
        );
    }
    let id = Uuid::new_v4();
    e.grpc
        .clone()
        .reserve_seats(e.request(ReserveSeatsRequest {
            flight_id: f.to_string(),
            booking_id: id.to_string(),
            seat_count: 2,
        }))
        .await
        .unwrap();
    assert_eq!(
        e.grpc
            .clone()
            .update_flight(e.request(UpdateFlightRequest {
                id: f.to_string(),
                total_seats: Some(1),
                price_minor: None,
                status: None
            }))
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    e.grpc
        .clone()
        .release_reservation(e.request(ReleaseReservationRequest {
            booking_id: id.to_string(),
        }))
        .await
        .unwrap();
    e.grpc
        .clone()
        .update_flight(e.request(UpdateFlightRequest {
            id: f.to_string(),
            total_seats: None,
            price_minor: None,
            status: Some(FlightStatus::Departed as i32),
        }))
        .await
        .unwrap();
    assert_eq!(
        e.create(f, 1, Uuid::new_v4(), Uuid::new_v4())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(e.flight(f).await.available_seats, 2);
}
#[tokio::test]
#[ignore = "requires Compose"]
async fn recovery_compensates_orphan_reservation_and_replays_cancellation() {
    let e = Env::new().await;
    let f = e.fixture(4).await;
    let id = Uuid::new_v4();
    e.grpc
        .clone()
        .reserve_seats(e.request(ReserveSeatsRequest {
            flight_id: f.to_string(),
            booking_id: id.to_string(),
            seat_count: 1,
        }))
        .await
        .unwrap();
    sqlx::query("INSERT INTO booking_operations(booking_id,request,state,created_at) VALUES($1,$2,'PENDING',now()-interval '60 seconds')")
        .bind(id).bind(e.body(f,1,Uuid::new_v4())).execute(&e.bookings).await.unwrap();
    for _ in 0..30 {
        let state: String =
            sqlx::query_scalar("SELECT state FROM booking_operations WHERE booking_id=$1")
                .bind(id)
                .fetch_one(&e.bookings)
                .await
                .unwrap();
        if state == "ABORTED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(e.flight(f).await.available_seats, 4);
    let confirmed = Uuid::new_v4();
    assert_eq!(
        e.create(f, 1, Uuid::new_v4(), confirmed).await.status(),
        StatusCode::CREATED
    );
    sqlx::query("INSERT INTO cancellation_operations(booking_id) VALUES($1)")
        .bind(confirmed)
        .execute(&e.bookings)
        .await
        .unwrap();
    e.grpc
        .clone()
        .release_reservation(e.request(ReleaseReservationRequest {
            booking_id: confirmed.to_string(),
        }))
        .await
        .unwrap();
    for _ in 0..30 {
        let state: String = sqlx::query_scalar("SELECT status FROM bookings WHERE id=$1")
            .bind(confirmed)
            .fetch_one(&e.bookings)
            .await
            .unwrap();
        if state == "CANCELLED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let state: String = sqlx::query_scalar("SELECT status FROM bookings WHERE id=$1")
        .bind(confirmed)
        .fetch_one(&e.bookings)
        .await
        .unwrap();
    assert_eq!(state, "CANCELLED");
    assert_eq!(e.flight(f).await.available_seats, 4);
}

#[tokio::test]
#[ignore = "requires Compose"]
async fn successful_remote_reservation_is_compensated_when_booking_write_fails() {
    let e = Env::new().await;
    let flight = e.fixture(3).await;
    let booking = Uuid::new_v4();
    let name = format!("test_reject_{}", flight.simple());
    sqlx::query(&format!(
        "ALTER TABLE bookings ADD CONSTRAINT {name} CHECK (flight_id <> '{flight}') NOT VALID"
    ))
    .execute(&e.bookings)
    .await
    .unwrap();
    let response = e.create(flight, 2, Uuid::new_v4(), booking).await;
    sqlx::query(&format!("ALTER TABLE bookings DROP CONSTRAINT {name}"))
        .execute(&e.bookings)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(e.flight(flight).await.available_seats, 3);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bookings WHERE id=$1")
        .bind(booking)
        .fetch_one(&e.bookings)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let status: String =
        sqlx::query_scalar("SELECT status FROM seat_reservations WHERE booking_id=$1")
            .bind(booking)
            .fetch_one(&e.flights)
            .await
            .unwrap();
    assert_eq!(status, "RELEASED");
}
