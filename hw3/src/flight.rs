use crate::{cache::Cache, pb::*};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use tonic::{Request, Response, Status};
use uuid::Uuid;

#[derive(Clone)]
pub struct Flights {
    pub db: PgPool,
    pub cache: Cache,
}

#[derive(sqlx::FromRow)]
struct FlightRow {
    id: Uuid,
    flight_number: String,
    airline_code: String,
    airline_name: String,
    origin: String,
    destination: String,
    departure_time: DateTime<Utc>,
    arrival_time: DateTime<Utc>,
    total_seats: i32,
    available_seats: i32,
    price_minor: i64,
    currency: String,
    status: String,
}
const FLIGHT_SELECT: &str =
    "SELECT f.*, a.name AS airline_name FROM flights f JOIN airlines a ON a.code=f.airline_code";
fn timestamp(value: DateTime<Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: value.timestamp(),
        nanos: value.timestamp_subsec_nanos() as i32,
    }
}
impl From<FlightRow> for Flight {
    fn from(f: FlightRow) -> Self {
        Self {
            id: f.id.to_string(),
            flight_number: f.flight_number,
            airline_code: f.airline_code,
            airline_name: f.airline_name,
            origin: f.origin,
            destination: f.destination,
            departure_time: Some(timestamp(f.departure_time)),
            arrival_time: Some(timestamp(f.arrival_time)),
            total_seats: f.total_seats,
            available_seats: f.available_seats,
            price_minor: f.price_minor,
            currency: f.currency,
            status: FlightStatus::from_str_name(&f.status).unwrap_or(FlightStatus::Unspecified)
                as i32,
        }
    }
}
#[derive(sqlx::FromRow)]
struct ReservationRow {
    id: Uuid,
    flight_id: Uuid,
    booking_id: Uuid,
    seat_count: i32,
    status: String,
    created_at: DateTime<Utc>,
}
impl From<ReservationRow> for SeatReservation {
    fn from(r: ReservationRow) -> Self {
        Self {
            id: r.id.to_string(),
            flight_id: r.flight_id.to_string(),
            booking_id: r.booking_id.to_string(),
            seat_count: r.seat_count,
            status: ReservationStatus::from_str_name(&r.status)
                .unwrap_or(ReservationStatus::Unspecified) as i32,
            created_at: Some(timestamp(r.created_at)),
        }
    }
}
pub fn id(raw: &str) -> Result<Uuid, Status> {
    Uuid::parse_str(raw).map_err(|_| Status::invalid_argument("expected a UUID"))
}
pub fn db_error(error: sqlx::Error) -> Status {
    tracing::error!(%error, "database operation failed");
    match error {
        sqlx::Error::RowNotFound => Status::not_found("flight not found"),
        _ => Status::unavailable("database temporarily unavailable"),
    }
}
fn airport(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())
}
async fn booking_lock(tx: &mut Transaction<'_, Postgres>, booking_id: Uuid) -> Result<(), Status> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(booking_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}
async fn invalidate(tx: &mut Transaction<'_, Postgres>) -> Result<(), Status> {
    sqlx::query("UPDATE cache_revision SET revision=revision+1")
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
    Ok(())
}
impl Flights {
    async fn revision(&self) -> Result<i64, Status> {
        sqlx::query_scalar("SELECT revision FROM cache_revision")
            .fetch_one(&self.db)
            .await
            .map_err(db_error)
    }
}
#[tonic::async_trait]
impl flight_service_server::FlightService for Flights {
    async fn search_flights(
        &self,
        request: Request<SearchFlightsRequest>,
    ) -> Result<Response<SearchFlightsResponse>, Status> {
        let r = request.into_inner();
        if !airport(&r.origin) || !airport(&r.destination) || r.origin == r.destination {
            return Err(Status::invalid_argument(
                "different three-letter uppercase IATA codes required",
            ));
        }
        let date = r
            .date
            .as_deref()
            .map(|date| {
                let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
                    .map_err(|_| Status::invalid_argument("date must be YYYY-MM-DD"))?;
                if parsed.format("%Y-%m-%d").to_string() != date {
                    return Err(Status::invalid_argument("date must be YYYY-MM-DD"));
                }
                Ok(parsed)
            })
            .transpose()?;
        let revision = self.revision().await?;
        let key = format!(
            "search:{}:{}:{}:v{}",
            r.origin,
            r.destination,
            r.date.as_deref().unwrap_or("all"),
            revision
        );
        if let Some(value) = self.cache.get(&key).await {
            return Ok(Response::new(value));
        }
        let rows=sqlx::query_as::<_,FlightRow>(&format!("{FLIGHT_SELECT} WHERE f.origin=$1 AND f.destination=$2 AND f.status='SCHEDULED' AND ($3::date IS NULL OR (f.departure_time AT TIME ZONE 'UTC')::date=$3) ORDER BY f.departure_time,f.id"))
            .bind(r.origin).bind(r.destination).bind(date).fetch_all(&self.db).await.map_err(db_error)?;
        let value = SearchFlightsResponse {
            flights: rows.into_iter().map(Into::into).collect(),
        };
        self.cache.put(&key, &value).await;
        Ok(Response::new(value))
    }
    async fn get_flight(
        &self,
        request: Request<GetFlightRequest>,
    ) -> Result<Response<Flight>, Status> {
        let flight_id = id(&request.into_inner().id)?;
        let key = format!("flight:{flight_id}:v{}", self.revision().await?);
        if let Some(value) = self.cache.get(&key).await {
            return Ok(Response::new(value));
        }
        let row = sqlx::query_as::<_, FlightRow>(&format!("{FLIGHT_SELECT} WHERE f.id=$1"))
            .bind(flight_id)
            .fetch_one(&self.db)
            .await
            .map_err(db_error)?;
        let value = Flight::from(row);
        self.cache.put(&key, &value).await;
        Ok(Response::new(value))
    }
    async fn reserve_seats(
        &self,
        request: Request<ReserveSeatsRequest>,
    ) -> Result<Response<SeatReservation>, Status> {
        let r = request.into_inner();
        let flight_id = id(&r.flight_id)?;
        let booking_id = id(&r.booking_id)?;
        if r.seat_count <= 0 {
            return Err(Status::invalid_argument("seat_count must be positive"));
        }
        let mut tx = self.db.begin().await.map_err(db_error)?;
        booking_lock(&mut tx, booking_id).await?;
        let fenced: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM released_booking_ids WHERE booking_id=$1)",
        )
        .bind(booking_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        if fenced {
            return Err(Status::failed_precondition("booking was released"));
        }
        if let Some(existing) = sqlx::query_as::<_, ReservationRow>(
            "SELECT * FROM seat_reservations WHERE booking_id=$1",
        )
        .bind(booking_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        {
            if existing.flight_id != flight_id || existing.seat_count != r.seat_count {
                return Err(Status::already_exists(
                    "booking_id has different reservation parameters",
                ));
            }
            if existing.status != "ACTIVE" {
                return Err(Status::failed_precondition("reservation is not active"));
            }
            return Ok(Response::new(existing.into()));
        }
        // Row lock serializes contenders for the last seat.
        let (available, status): (i32, String) =
            sqlx::query_as("SELECT available_seats,status FROM flights WHERE id=$1 FOR UPDATE")
                .bind(flight_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if status != "SCHEDULED" {
            return Err(Status::failed_precondition("flight is not scheduled"));
        }
        if available < r.seat_count {
            return Err(Status::resource_exhausted("not enough available seats"));
        }
        sqlx::query("UPDATE flights SET available_seats=available_seats-$2 WHERE id=$1")
            .bind(flight_id)
            .bind(r.seat_count)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        let row=sqlx::query_as::<_,ReservationRow>("INSERT INTO seat_reservations(id,flight_id,booking_id,seat_count,status) VALUES($1,$2,$3,$4,'ACTIVE') RETURNING *")
            .bind(Uuid::new_v4()).bind(flight_id).bind(booking_id).bind(r.seat_count).fetch_one(&mut *tx).await.map_err(db_error)?;
        invalidate(&mut tx).await?;
        tx.commit().await.map_err(db_error)?;
        tracing::info!(%booking_id,%flight_id,event="seats_reserved");
        Ok(Response::new(row.into()))
    }
    async fn release_reservation(
        &self,
        request: Request<ReleaseReservationRequest>,
    ) -> Result<Response<ReleaseReservationResponse>, Status> {
        let booking_id = id(&request.into_inner().booking_id)?;
        let mut tx = self.db.begin().await.map_err(db_error)?;
        booking_lock(&mut tx, booking_id).await?;
        sqlx::query(
            "INSERT INTO released_booking_ids(booking_id) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .bind(booking_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
        if let Some(row) = sqlx::query_as::<_, ReservationRow>(
            "SELECT * FROM seat_reservations WHERE booking_id=$1 FOR UPDATE",
        )
        .bind(booking_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
            && row.status == "ACTIVE"
        {
            sqlx::query("UPDATE flights SET available_seats=available_seats+$2 WHERE id=$1")
                .bind(row.flight_id)
                .bind(row.seat_count)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            sqlx::query("UPDATE seat_reservations SET status='RELEASED' WHERE booking_id=$1")
                .bind(booking_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            invalidate(&mut tx).await?;
        }
        tx.commit().await.map_err(db_error)?;
        tracing::info!(%booking_id,event="reservation_released");
        Ok(Response::new(ReleaseReservationResponse {
            status: ReservationStatus::Released as i32,
        }))
    }
    async fn update_flight(
        &self,
        request: Request<UpdateFlightRequest>,
    ) -> Result<Response<Flight>, Status> {
        let r = request.into_inner();
        let flight_id = id(&r.id)?;
        if r.price_minor.is_some_and(|p| p <= 0) || r.total_seats.is_some_and(|s| s <= 0) {
            return Err(Status::invalid_argument(
                "price and total_seats must be positive",
            ));
        }
        let status = r
            .status
            .map(|s| {
                FlightStatus::try_from(s)
                    .ok()
                    .filter(|s| *s != FlightStatus::Unspecified)
                    .map(|s| s.as_str_name())
                    .ok_or_else(|| Status::invalid_argument("invalid flight status"))
            })
            .transpose()?;
        let mut tx = self.db.begin().await.map_err(db_error)?;
        let (total, available): (i32, i32) = sqlx::query_as(
            "SELECT total_seats,available_seats FROM flights WHERE id=$1 FOR UPDATE",
        )
        .bind(flight_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?;
        let new_total = r.total_seats.unwrap_or(total);
        let reserved = total - available;
        if new_total < reserved {
            return Err(Status::failed_precondition(
                "capacity is below reserved seat count",
            ));
        }
        sqlx::query("UPDATE flights SET total_seats=$2,available_seats=$3,price_minor=COALESCE($4,price_minor),status=COALESCE($5,status) WHERE id=$1")
            .bind(flight_id).bind(new_total).bind(new_total-reserved).bind(r.price_minor).bind(status)
            .execute(&mut *tx).await.map_err(db_error)?;
        invalidate(&mut tx).await?;
        let row = sqlx::query_as::<_, FlightRow>(&format!("{FLIGHT_SELECT} WHERE f.id=$1"))
            .bind(flight_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        Ok(Response::new(row.into()))
    }
}
pub fn authenticate(key: String) -> impl FnMut(Request<()>) -> Result<Request<()>, Status> + Clone {
    move |request| {
        let received = request
            .metadata()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mismatch = received
            .as_bytes()
            .iter()
            .zip(key.as_bytes())
            .fold(0_u8, |acc, (a, b)| acc | (a ^ b));
        if received.len() != key.len() || mismatch != 0 || key.is_empty() {
            return Err(Status::unauthenticated("valid x-api-key metadata required"));
        }
        Ok(request)
    }
}
