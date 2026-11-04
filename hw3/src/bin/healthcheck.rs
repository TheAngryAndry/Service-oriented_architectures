use flight_booking::pb::{GetFlightRequest, flight_service_client::FlightServiceClient};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = FlightServiceClient::connect("http://127.0.0.1:50051").await?;
    let mut request = tonic::Request::new(GetFlightRequest {
        id: "11111111-1111-4111-8111-111111111111".into(),
    });
    request
        .metadata_mut()
        .insert("x-api-key", std::env::var("GRPC_API_KEY")?.parse()?);
    request.set_timeout(std::time::Duration::from_secs(2));
    client.get_flight(request).await?;
    Ok(())
}
