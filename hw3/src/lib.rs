pub mod booking;
pub mod cache;
pub mod client;
pub mod flight;
pub mod settings;
pub mod pb {
    tonic::include_proto!("flight.v1");
}
