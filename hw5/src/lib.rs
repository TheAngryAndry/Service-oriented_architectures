use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub const TYPES: [&str; 6] = [
    "VIEW_STARTED",
    "VIEW_FINISHED",
    "VIEW_PAUSED",
    "VIEW_RESUMED",
    "LIKED",
    "SEARCHED",
];
pub const DEVICES: [&str; 4] = ["MOBILE", "DESKTOP", "TV", "TABLET"];
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub event_id: uuid::Uuid,
    pub user_id: String,
    pub movie_id: String,
    pub event_type: String,
    pub timestamp: DateTime<Utc>,
    pub device_type: String,
    pub session_id: String,
    pub progress_seconds: i32,
}
impl Event {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.event_id.is_nil(), "event_id must not be nil");
        for s in [&self.user_id, &self.movie_id, &self.session_id] {
            anyhow::ensure!(
                !s.trim().is_empty() && s.len() <= 256,
                "identifiers must contain 1..256 bytes"
            );
        }
        anyhow::ensure!(
            TYPES.contains(&self.event_type.as_str()),
            "invalid event_type"
        );
        anyhow::ensure!(
            DEVICES.contains(&self.device_type.as_str()),
            "invalid device_type"
        );
        anyhow::ensure!(self.progress_seconds >= 0, "negative progress");
        anyhow::ensure!(
            self.timestamp.timestamp_millis() >= 0,
            "timestamp before epoch"
        );
        Ok(())
    }
    pub fn avro(&self) -> anyhow::Result<apache_avro::types::Value> {
        use apache_avro::types::Value as V;
        self.validate()?;
        Ok(V::Record(vec![
            ("event_id".into(), V::String(self.event_id.to_string())),
            ("user_id".into(), V::String(self.user_id.clone())),
            ("movie_id".into(), V::String(self.movie_id.clone())),
            (
                "event_type".into(),
                V::Enum(
                    TYPES.iter().position(|s| *s == self.event_type).unwrap() as u32,
                    self.event_type.clone(),
                ),
            ),
            (
                "timestamp".into(),
                V::TimestampMillis(self.timestamp.timestamp_millis()),
            ),
            (
                "device_type".into(),
                V::Enum(
                    DEVICES.iter().position(|s| *s == self.device_type).unwrap() as u32,
                    self.device_type.clone(),
                ),
            ),
            ("session_id".into(), V::String(self.session_id.clone())),
            ("progress_seconds".into(), V::Int(self.progress_seconds)),
        ]))
    }
}
pub fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}
pub fn logs() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(env("RUST_LOG", "info"))
        .init();
}
pub async fn ch(sql: &str) -> anyhow::Result<String> {
    let response = reqwest::Client::new()
        .post(env("CLICKHOUSE_URL", "http://clickhouse:8123"))
        .basic_auth("default", Some(env("CLICKHOUSE_PASSWORD", "cinema-local")))
        .timeout(std::time::Duration::from_secs(60))
        .body(sql.to_owned())
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    anyhow::ensure!(status.is_success(), "ClickHouse {status}: {body}");
    Ok(body)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_and_encodes_schema() {
        let mut e = Event {
            event_id: uuid::Uuid::new_v4(),
            user_id: "u".into(),
            movie_id: "m".into(),
            event_type: TYPES[0].into(),
            timestamp: Utc::now(),
            device_type: "TV".into(),
            session_id: "s".into(),
            progress_seconds: 0,
        };
        let schema =
            apache_avro::Schema::parse_str(include_str!("../schemas/movie-event-v1.avsc")).unwrap();
        assert!(apache_avro::to_avro_datum(&schema, e.avro().unwrap()).is_ok());
        e.progress_seconds = -1;
        assert!(e.validate().is_err());
        e.progress_seconds = 0;
        e.event_type = "UNKNOWN".into();
        assert!(e.validate().is_err());
    }
    #[test]
    fn all_enum_variants_roundtrip_and_invalid_identifiers_fail() {
        let schema =
            apache_avro::Schema::parse_str(include_str!("../schemas/movie-event-v1.avsc")).unwrap();
        let mut e = Event {
            event_id: uuid::Uuid::new_v4(),
            user_id: "u".into(),
            movie_id: "m".into(),
            event_type: TYPES[0].into(),
            timestamp: Utc::now(),
            device_type: DEVICES[0].into(),
            session_id: "s".into(),
            progress_seconds: 0,
        };
        for kind in TYPES {
            for device in DEVICES {
                e.event_type = kind.into();
                e.device_type = device.into();
                let expected = e.avro().unwrap();
                let bytes = apache_avro::to_avro_datum(&schema, expected.clone()).unwrap();
                let actual =
                    apache_avro::from_avro_datum(&schema, &mut bytes.as_slice(), None).unwrap();
                assert_eq!(actual, expected);
            }
        }
        e.user_id = " ".into();
        assert!(e.validate().is_err());
        e.user_id = "u".repeat(257);
        assert!(e.validate().is_err());
        e.user_id = "u".into();
        e.event_id = uuid::Uuid::nil();
        assert!(e.validate().is_err());
    }
}
