//! Outbound gRPC middleware. Business handlers never implement retry/breaker logic.
use crate::pb::{self, flight_service_client::FlightServiceClient};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use tonic::{
    Code, Request, Response, Status,
    transport::{Channel, Endpoint},
};

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub timeout: Duration,
    pub retry_base: Duration,
    pub failure_threshold: u32,
    pub open_duration: Duration,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(2),
            retry_base: Duration::from_millis(100),
            failure_threshold: 5,
            open_duration: Duration::from_secs(10),
        }
    }
}
enum State {
    Closed { failures: u32 },
    Open { until: Instant },
    HalfOpen,
}
struct Breaker {
    state: State,
    generation: u64,
    config: ClientConfig,
}
impl Breaker {
    fn transition(&mut self, state: State, from: &str, to: &str) {
        tracing::warn!(event="circuit_transition",%from,%to);
        self.state = state;
        self.generation += 1;
    }
}
struct Permit {
    breaker: Arc<Mutex<Breaker>>,
    generation: u64,
    probe: bool,
    finished: bool,
}
impl Permit {
    fn finish(&mut self, failed: bool) {
        self.finished = true;
        let mut b = self.breaker.lock().unwrap();
        if b.generation != self.generation {
            return;
        }
        match b.state {
            State::HalfOpen if self.probe => {
                if failed {
                    let until = Instant::now() + b.config.open_duration;
                    b.transition(State::Open { until }, "HALF_OPEN", "OPEN");
                } else {
                    b.transition(State::Closed { failures: 0 }, "HALF_OPEN", "CLOSED");
                }
            }
            State::Closed { failures } => {
                let failures = if failed { failures + 1 } else { 0 };
                if failures >= b.config.failure_threshold {
                    let until = Instant::now() + b.config.open_duration;
                    b.transition(State::Open { until }, "CLOSED", "OPEN");
                } else {
                    b.state = State::Closed { failures };
                }
            }
            _ => {}
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        // Cancellation must not leave HALF_OPEN occupied forever.
        if !self.finished {
            self.finish(true);
        }
    }
}
#[derive(Clone)]
pub struct FlightClient {
    channel: Channel,
    key: String,
    config: ClientConfig,
    breaker: Arc<Mutex<Breaker>>,
}
pub fn retryable(code: Code) -> bool {
    matches!(code, Code::Unavailable | Code::DeadlineExceeded)
}
impl FlightClient {
    pub fn new(
        url: String,
        key: String,
        config: ClientConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if config.failure_threshold == 0
            || config.timeout.is_zero()
            || config.open_duration.is_zero()
        {
            return Err("circuit configuration must be positive".into());
        }
        // Validate metadata at startup rather than panic on a request.
        key.parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>()?;
        let channel = Endpoint::from_shared(url)?
            .connect_timeout(config.timeout)
            .connect_lazy();
        let breaker = Arc::new(Mutex::new(Breaker {
            state: State::Closed { failures: 0 },
            generation: 0,
            config: config.clone(),
        }));
        Ok(Self {
            channel,
            key,
            config,
            breaker,
        })
    }
    fn acquire(&self) -> Result<Permit, Status> {
        let mut b = self.breaker.lock().unwrap();
        let probe = match b.state {
            State::Closed { .. } => false,
            State::Open { until } if Instant::now() >= until => {
                b.transition(State::HalfOpen, "OPEN", "HALF_OPEN");
                true
            }
            _ => {
                tracing::warn!(event = "circuit_rejected");
                return Err(Status::unavailable(
                    "Flight Service circuit is open; retry later",
                ));
            }
        };
        Ok(Permit {
            breaker: self.breaker.clone(),
            generation: b.generation,
            probe,
            finished: false,
        })
    }
    async fn call<T: Clone, R, F, Fut>(
        &self,
        method: &str,
        message: T,
        operation: F,
    ) -> Result<R, Status>
    where
        F: Fn(FlightServiceClient<Channel>, Request<T>) -> Fut,
        Fut: Future<Output = Result<Response<R>, Status>>,
    {
        let mut permit = self.acquire()?;
        let attempts = if permit.probe { 1 } else { 3 };
        for attempt in 1..=attempts {
            let mut request = Request::new(message.clone());
            request
                .metadata_mut()
                .insert("x-api-key", self.key.parse().expect("validated at startup"));
            // Own the per-attempt deadline: Tonic maps grpc-timeout expiry to CANCELLED.
            // The outer timer returns DEADLINE_EXCEEDED and dropping the call cancels the stream.
            tracing::info!(event="grpc_attempt",%method,attempt);
            let result = tokio::time::timeout(
                self.config.timeout,
                operation(FlightServiceClient::new(self.channel.clone()), request),
            )
            .await
            .unwrap_or_else(|_| {
                Err(Status::deadline_exceeded(
                    "Flight Service deadline exceeded",
                ))
            });
            match result {
                Ok(response) => {
                    permit.finish(false);
                    return Ok(response.into_inner());
                }
                Err(status) => {
                    let transient = retryable(status.code());
                    if !transient || attempt == attempts {
                        permit.finish(transient);
                        return Err(status);
                    }
                    let delay = self.config.retry_base.saturating_mul(1 << (attempt - 1));
                    tracing::warn!(event="grpc_retry",%method,attempt,code=?status.code(),delay_ms=delay.as_millis() as u64);
                    tokio::time::sleep(delay).await;
                }
            }
        }
        unreachable!()
    }
    pub async fn get(&self, id: String) -> Result<pb::Flight, Status> {
        self.call(
            "GetFlight",
            pb::GetFlightRequest { id },
            |mut c, r| async move { c.get_flight(r).await },
        )
        .await
    }
    pub async fn search(
        &self,
        r: pb::SearchFlightsRequest,
    ) -> Result<pb::SearchFlightsResponse, Status> {
        self.call("SearchFlights", r, |mut c, r| async move {
            c.search_flights(r).await
        })
        .await
    }
    pub async fn reserve(&self, r: pb::ReserveSeatsRequest) -> Result<pb::SeatReservation, Status> {
        self.call("ReserveSeats", r, |mut c, r| async move {
            c.reserve_seats(r).await
        })
        .await
    }
    pub async fn release(&self, id: String) -> Result<pb::ReleaseReservationResponse, Status> {
        self.call(
            "ReleaseReservation",
            pb::ReleaseReservationRequest { booking_id: id },
            |mut c, r| async move { c.release_reservation(r).await },
        )
        .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_transport_errors_are_retried() {
        for code in [
            Code::InvalidArgument,
            Code::NotFound,
            Code::ResourceExhausted,
            Code::Unauthenticated,
            Code::Internal,
        ] {
            assert!(!retryable(code));
        }
        assert!(retryable(Code::Unavailable));
        assert!(retryable(Code::DeadlineExceeded));
    }
    #[tokio::test]
    async fn one_half_open_probe_and_cancelled_probe_reopens() {
        let client = FlightClient::new(
            "http://127.0.0.1:1".into(),
            "test".into(),
            ClientConfig {
                failure_threshold: 1,
                open_duration: Duration::from_millis(10),
                ..Default::default()
            },
        )
        .unwrap();
        client.acquire().unwrap().finish(true);
        assert!(client.acquire().is_err());
        tokio::time::sleep(Duration::from_millis(15)).await;
        let probe = client.acquire().unwrap();
        assert!(probe.probe);
        assert!(client.acquire().is_err());
        drop(probe);
        assert!(client.acquire().is_err());
        tokio::time::sleep(Duration::from_millis(15)).await;
        client.acquire().unwrap().finish(false);
        assert!(!client.acquire().unwrap().probe);
    }
    #[tokio::test]
    async fn stale_inflight_success_cannot_close_open_breaker() {
        let client = FlightClient::new(
            "http://127.0.0.1:1".into(),
            "test".into(),
            ClientConfig {
                failure_threshold: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut old = client.acquire().unwrap();
        client.acquire().unwrap().finish(true);
        old.finish(false);
        assert!(client.acquire().is_err());
    }
}
