use flight_booking::{
    client::{ClientConfig, FlightClient},
    pb::{
        flight_service_server::{FlightService, FlightServiceServer},
        *,
    },
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Code, Request, Response, Status};

#[derive(Clone)]
struct Fake {
    outcomes: Arc<Mutex<VecDeque<Code>>>,
    calls: Arc<AtomicUsize>,
    delay: Duration,
    booking_ids: Arc<Mutex<Vec<String>>>,
}
impl Fake {
    fn new(codes: Vec<Code>) -> Self {
        Self {
            outcomes: Arc::new(Mutex::new(codes.into())),
            calls: Arc::new(AtomicUsize::new(0)),
            delay: Duration::ZERO,
            booking_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }
    async fn outcome(&self) -> Result<(), Status> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        let code = self
            .outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Code::Ok);
        if code == Code::Ok {
            Ok(())
        } else {
            Err(Status::new(code, "injected failure"))
        }
    }
}
#[tonic::async_trait]
impl FlightService for Fake {
    async fn get_flight(&self, _: Request<GetFlightRequest>) -> Result<Response<Flight>, Status> {
        self.outcome().await?;
        Ok(Response::new(Flight::default()))
    }
    async fn reserve_seats(
        &self,
        r: Request<ReserveSeatsRequest>,
    ) -> Result<Response<SeatReservation>, Status> {
        self.booking_ids
            .lock()
            .unwrap()
            .push(r.into_inner().booking_id);
        self.outcome().await?;
        Ok(Response::new(SeatReservation::default()))
    }
    async fn search_flights(
        &self,
        _: Request<SearchFlightsRequest>,
    ) -> Result<Response<SearchFlightsResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn release_reservation(
        &self,
        _: Request<ReleaseReservationRequest>,
    ) -> Result<Response<ReleaseReservationResponse>, Status> {
        Err(Status::unimplemented("unused"))
    }
    async fn update_flight(
        &self,
        _: Request<UpdateFlightRequest>,
    ) -> Result<Response<Flight>, Status> {
        Err(Status::unimplemented("unused"))
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn start(fake: Fake, config: ClientConfig) -> (FlightClient, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(FlightServiceServer::new(fake))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let client = FlightClient::new(format!("http://{address}"), "test".into(), config).unwrap();
    (client, Server(task))
}
fn config() -> ClientConfig {
    ClientConfig {
        timeout: Duration::from_secs(1),
        retry_base: Duration::from_millis(5),
        failure_threshold: 1,
        open_duration: Duration::from_millis(30),
    }
}
#[tokio::test]
async fn retry_uses_at_most_three_attempts_and_exponential_backoff() {
    let fake = Fake::new(vec![Code::Unavailable, Code::DeadlineExceeded, Code::Ok]);
    let (client, _server) = start(fake.clone(), config()).await;
    let now = std::time::Instant::now();
    client.get("id".into()).await.unwrap();
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    assert!(now.elapsed() >= Duration::from_millis(15));
    let fake = Fake::new(vec![Code::Unavailable; 4]);
    let (client, _server) = start(fake.clone(), config()).await;
    assert_eq!(
        client.get("id".into()).await.unwrap_err().code(),
        Code::Unavailable
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
}
#[tokio::test]
async fn business_and_authentication_errors_are_never_retried() {
    for code in [
        Code::InvalidArgument,
        Code::NotFound,
        Code::ResourceExhausted,
        Code::Unauthenticated,
        Code::AlreadyExists,
        Code::FailedPrecondition,
    ] {
        let fake = Fake::new(vec![code]);
        let (client, _server) = start(fake.clone(), config()).await;
        assert_eq!(client.get("id".into()).await.unwrap_err().code(), code);
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
        // A business error must not open the breaker.
        client.get("id".into()).await.unwrap();
        assert_eq!(fake.calls.load(Ordering::SeqCst), 2);
    }
}
#[tokio::test]
async fn reserve_retries_keep_exactly_the_same_booking_id() {
    let fake = Fake::new(vec![Code::Unavailable, Code::Ok]);
    let (client, _server) = start(fake.clone(), config()).await;
    client
        .reserve(ReserveSeatsRequest {
            flight_id: "flight".into(),
            booking_id: "stable-id".into(),
            seat_count: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        *fake.booking_ids.lock().unwrap(),
        vec!["stable-id", "stable-id"]
    );
}
#[tokio::test]
async fn breaker_rejects_without_network_then_probes_and_recovers() {
    let fake = Fake::new(vec![Code::Unavailable; 4]);
    let (client, _server) = start(fake.clone(), config()).await;
    assert!(client.get("id".into()).await.is_err());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    let now = std::time::Instant::now();
    assert!(
        client
            .get("id".into())
            .await
            .unwrap_err()
            .message()
            .contains("circuit")
    );
    assert!(now.elapsed() < Duration::from_millis(20));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(client.get("id".into()).await.is_err()); // one failed HALF_OPEN probe
    assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
    assert!(client.get("id".into()).await.is_err());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 4);
    tokio::time::sleep(Duration::from_millis(40)).await;
    client.get("id".into()).await.unwrap(); // successful probe
    client.get("id".into()).await.unwrap(); // CLOSED
    assert_eq!(fake.calls.load(Ordering::SeqCst), 6);
}
#[tokio::test]
async fn slow_server_is_bounded_by_deadline() {
    let mut fake = Fake::new(vec![]);
    fake.delay = Duration::from_millis(100);
    let mut cfg = config();
    cfg.timeout = Duration::from_millis(20);
    let (client, _server) = start(fake.clone(), cfg).await;
    let now = std::time::Instant::now();
    let error = client.get("id".into()).await.unwrap_err();
    assert_eq!(error.code(), Code::DeadlineExceeded);
    assert!(now.elapsed() < Duration::from_millis(300));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 3);
}
