use crate::{client::FlightClient, flight, pb};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tonic::{Code, Status};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub flights: FlightClient,
}
pub struct ApiError(Status);
impl From<Status> for ApiError {
    fn from(e: Status) -> Self {
        Self(e)
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self(flight::db_error(e))
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.code() {
            Code::InvalidArgument => StatusCode::BAD_REQUEST,
            Code::NotFound => StatusCode::NOT_FOUND,
            Code::AlreadyExists | Code::FailedPrecondition | Code::ResourceExhausted => {
                StatusCode::CONFLICT
            }
            Code::Unavailable | Code::DeadlineExceeded | Code::Unauthenticated => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(
                json!({"error":{"code":format!("{:?}",self.0.code()),"message":self.0.message()}}),
            ),
        )
            .into_response()
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CreateBooking {
    pub user_id: Uuid,
    pub flight_id: Uuid,
    pub passenger_name: String,
    pub passenger_email: String,
    pub seat_count: i32,
}
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub user_id: Uuid,
    pub flight_id: Uuid,
    pub passenger_name: String,
    pub passenger_email: String,
    pub seat_count: i32,
    pub total_price: i64,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}
impl CreateBooking {
    fn validate(&self) -> Result<(), ApiError> {
        let email = self.passenger_email.as_str();
        if self.seat_count <= 0
            || self.passenger_name.trim().is_empty()
            || self.passenger_name.chars().count() > 200
            || email.len() > 254
            || email.chars().any(char::is_whitespace)
            || !email.split_once('@').is_some_and(|(name, domain)| {
                !name.is_empty()
                    && domain.contains('.')
                    && !domain.contains('@')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
            })
        {
            return Err(Status::invalid_argument(
                "positive seat_count, passenger name and valid email required",
            )
            .into());
        }
        Ok(())
    }
}
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/flights", get(search))
        .route("/flights/{id}", get(get_flight))
        .route("/bookings", post(create).get(list))
        .route("/bookings/{id}", get(get_booking))
        .route("/bookings/{id}/cancel", post(cancel))
        .with_state(state)
}
async fn health(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1").execute(&s.db).await?;
    Ok(Json(json!({"status":"ok"})))
}
fn flight_json(f: pb::Flight) -> Value {
    let time = |v: Option<prost_types::Timestamp>| {
        v.and_then(|v| DateTime::<Utc>::from_timestamp(v.seconds, v.nanos as u32))
            .map(|v| v.to_rfc3339())
    };
    json!({"id":f.id,"flight_number":f.flight_number,"airline_code":f.airline_code,"airline_name":f.airline_name,
        "origin":f.origin,"destination":f.destination,"departure_time":time(f.departure_time),
        "arrival_time":time(f.arrival_time),"total_seats":f.total_seats,"available_seats":f.available_seats,
        "price_minor":f.price_minor,"currency":f.currency,
        "status":pb::FlightStatus::try_from(f.status).map(|s|s.as_str_name()).unwrap_or("UNKNOWN")})
}
#[derive(Deserialize)]
struct Search {
    origin: Option<String>,
    destination: Option<String>,
    date: Option<String>,
}
async fn search(
    State(s): State<AppState>,
    Query(q): Query<Search>,
) -> Result<Json<Value>, ApiError> {
    let response = s
        .flights
        .search(pb::SearchFlightsRequest {
            origin: q
                .origin
                .ok_or_else(|| Status::invalid_argument("origin is required"))?,
            destination: q
                .destination
                .ok_or_else(|| Status::invalid_argument("destination is required"))?,
            date: q.date,
        })
        .await?;
    Ok(Json(
        json!({"flights":response.flights.into_iter().map(flight_json).collect::<Vec<_>>()}),
    ))
}
async fn get_flight(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(flight_json(s.flights.get(id).await?)))
}
async fn get_booking(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Booking>, ApiError> {
    Ok(Json(load_booking(&s.db, flight::id(&id)?).await?))
}
async fn load_booking(db: &PgPool, id: Uuid) -> Result<Booking, ApiError> {
    sqlx::query_as("SELECT * FROM bookings WHERE id=$1")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| Status::not_found("booking not found").into())
}
#[derive(Deserialize)]
struct UserQuery {
    user_id: Option<String>,
}
async fn list(
    State(s): State<AppState>,
    Query(q): Query<UserQuery>,
) -> Result<Json<Vec<Booking>>, ApiError> {
    let user = flight::id(
        &q.user_id
            .ok_or_else(|| Status::invalid_argument("user_id is required"))?,
    )?;
    Ok(Json(
        sqlx::query_as("SELECT * FROM bookings WHERE user_id=$1 ORDER BY created_at,id")
            .bind(user)
            .fetch_all(&s.db)
            .await?,
    ))
}
async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CreateBooking>, JsonRejection>,
) -> Result<(StatusCode, Json<Booking>), ApiError> {
    let Json(body) = body.map_err(|e| Status::invalid_argument(e.body_text()))?;
    body.validate()?;
    let booking_id = match headers.get("Idempotency-Key") {
        Some(value) => flight::id(
            value
                .to_str()
                .map_err(|_| Status::invalid_argument("Idempotency-Key must be a UUID"))?,
        )?,
        None => Uuid::new_v4(),
    };
    let request = serde_json::to_value(&body).expect("serializable");
    // Record intent durably before a mutating remote call. Same ID is used on every retry.
    sqlx::query("INSERT INTO booking_operations(booking_id,request,state) VALUES($1,$2,'PENDING') ON CONFLICT DO NOTHING")
        .bind(booking_id).bind(&request).execute(&s.db).await?;
    let mut tx = s.db.begin().await?;
    let (saved, state): (Value, String) = sqlx::query_as(
        "SELECT request,state FROM booking_operations WHERE booking_id=$1 FOR UPDATE",
    )
    .bind(booking_id)
    .fetch_one(&mut *tx)
    .await?;
    if saved != request {
        return Err(
            Status::already_exists("Idempotency-Key was used for a different request").into(),
        );
    }
    if state == "DONE" {
        return Ok((StatusCode::OK, Json(load_booking(&s.db, booking_id).await?)));
    }
    if state != "PENDING" {
        return Err(Status::failed_precondition(
            "booking attempt was aborted; use a new Idempotency-Key",
        )
        .into());
    }
    let result = async {
        let flight = s.flights.get(body.flight_id.to_string()).await?;
        let total_price = flight
            .price_minor
            .checked_mul(i64::from(body.seat_count))
            .filter(|v| *v > 0)
            .ok_or_else(|| Status::invalid_argument("total price overflow"))?;
        s.flights
            .reserve(pb::ReserveSeatsRequest {
                flight_id: body.flight_id.to_string(),
                booking_id: booking_id.to_string(),
                seat_count: body.seat_count,
            })
            .await?;
        Ok::<_, Status>((total_price, flight.currency))
    }
    .await;
    let (total_price, currency) = match result {
        Ok(value) => value,
        Err(error) => {
            sqlx::query("UPDATE booking_operations SET state='COMPENSATE' WHERE booking_id=$1")
                .bind(booking_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            // Best effort now; durable worker retries after outages or process restart.
            let _ = compensate_one(&s, booking_id).await;
            return Err(error.into());
        }
    };
    let insert=sqlx::query_as::<_,Booking>("INSERT INTO bookings(id,user_id,flight_id,passenger_name,passenger_email,seat_count,total_price,currency,status) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'CONFIRMED') RETURNING *")
        .bind(booking_id).bind(body.user_id).bind(body.flight_id).bind(body.passenger_name).bind(body.passenger_email)
        .bind(body.seat_count).bind(total_price).bind(currency).fetch_one(&mut *tx).await;
    let booking = match insert {
        Ok(booking) => booking,
        Err(error) => {
            tx.rollback().await?;
            // The previously committed PENDING intent survives even if this update fails.
            let _ =
                sqlx::query("UPDATE booking_operations SET state='COMPENSATE' WHERE booking_id=$1")
                    .bind(booking_id)
                    .execute(&s.db)
                    .await;
            let _ = compensate_one(&s, booking_id).await;
            return Err(error.into());
        }
    };
    sqlx::query("UPDATE booking_operations SET state='DONE' WHERE booking_id=$1")
        .bind(booking_id)
        .execute(&mut *tx)
        .await?;
    // Unknown commit result is reconciled by the journal: DONE or stale PENDING, never blind release.
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(booking)))
}
async fn cancel(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Booking>, ApiError> {
    let id = flight::id(&id)?;
    load_booking(&s.db, id).await?;
    sqlx::query(
        "INSERT INTO cancellation_operations(booking_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .execute(&s.db)
    .await?;
    Ok(Json(cancel_booking(&s, id).await?))
}
async fn cancel_booking(s: &AppState, id: Uuid) -> Result<Booking, ApiError> {
    let mut tx = s.db.begin().await?;
    let booking: Booking = sqlx::query_as("SELECT * FROM bookings WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let booking = if booking.status == "CANCELLED" {
        booking
    } else {
        s.flights.release(id.to_string()).await?;
        sqlx::query_as("UPDATE bookings SET status='CANCELLED' WHERE id=$1 RETURNING *")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?
    };
    sqlx::query("DELETE FROM cancellation_operations WHERE booking_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(booking)
}
async fn compensate_one(s: &AppState, id: Uuid) -> Result<(), ApiError> {
    let mut tx = s.db.begin().await?;
    let state: Option<String> = sqlx::query_scalar(
        "SELECT state FROM booking_operations WHERE booking_id=$1 FOR UPDATE SKIP LOCKED",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if matches!(state.as_deref(), Some("COMPENSATE") | Some("PENDING")) {
        s.flights.release(id.to_string()).await?;
        sqlx::query("UPDATE booking_operations SET state='ABORTED' WHERE booking_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(%id,event="booking_compensated");
    }
    Ok(())
}
pub async fn recovery_worker(s: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        interval.tick().await;
        match sqlx::query_scalar::<_, Uuid>(
            "SELECT booking_id FROM cancellation_operations ORDER BY created_at LIMIT 100",
        )
        .fetch_all(&s.db)
        .await
        {
            Ok(ids) => {
                for id in ids {
                    if let Err(error) = cancel_booking(&s, id).await {
                        tracing::warn!(%id,error=%error.0,event="cancellation_pending");
                    }
                }
            }
            Err(error) => tracing::error!(%error,event="cancellation_recovery_failed"),
        }
        let pending=sqlx::query_scalar::<_,Uuid>("SELECT booking_id FROM booking_operations WHERE state='COMPENSATE' OR (state='PENDING' AND created_at<now()-interval '30 seconds') ORDER BY created_at LIMIT 100")
            .fetch_all(&s.db).await;
        match pending {
            Ok(ids) => {
                for id in ids {
                    if let Err(error) = compensate_one(&s, id).await {
                        tracing::warn!(%id,error=%error.0,event="compensation_pending");
                    }
                }
            }
            Err(error) => tracing::error!(%error,event="recovery_query_failed"),
        }
    }
}
