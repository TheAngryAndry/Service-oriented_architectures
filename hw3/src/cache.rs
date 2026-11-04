use prost::Message;
use redis::{cluster::ClusterClient, cluster_async::ClusterConnection};
use std::{sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Cache {
    client: ClusterClient,
    connection: Arc<Mutex<Option<ClusterConnection>>>,
    ttl: u64,
    timeout: Duration,
}
impl Cache {
    pub fn new(nodes: Vec<String>, ttl: u64, timeout: Duration) -> redis::RedisResult<Self> {
        Ok(Self {
            client: ClusterClient::new(nodes)?,
            connection: Arc::new(Mutex::new(None)),
            ttl,
            timeout,
        })
    }
    async fn connection(&self) -> Result<ClusterConnection, String> {
        let mut guard = self.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            return Ok(connection.clone());
        }
        let connection = self
            .client
            .get_async_connection()
            .await
            .map_err(|e| e.to_string())?;
        *guard = Some(connection.clone());
        Ok(connection)
    }
    pub async fn get<M: Message + Default>(&self, key: &str) -> Option<M> {
        let result = tokio::time::timeout(self.timeout, async {
            let mut connection = self.connection().await?;
            redis::cmd("GET")
                .arg(key)
                .query_async::<Option<Vec<u8>>>(&mut connection)
                .await
                .map_err(|e| e.to_string())
        })
        .await;
        match result {
            Ok(Ok(Some(bytes))) => match M::decode(bytes.as_slice()) {
                Ok(value) => {
                    tracing::info!(event="cache_hit", %key);
                    return Some(value);
                }
                Err(error) => tracing::warn!(event="cache_decode_error", %error),
            },
            Ok(Ok(None)) => {}
            Ok(Err(error)) => tracing::warn!(event="cache_unavailable", %error),
            Err(_) => tracing::warn!(event = "cache_timeout"),
        }
        tracing::info!(event="cache_miss", %key);
        None
    }
    pub async fn put<M: Message>(&self, key: &str, value: &M) {
        let bytes = value.encode_to_vec();
        let result = tokio::time::timeout(self.timeout, async {
            let mut connection = self.connection().await?;
            redis::cmd("SET")
                .arg(key)
                .arg(bytes)
                .arg("EX")
                .arg(self.ttl)
                .query_async::<()>(&mut connection)
                .await
                .map_err(|e| e.to_string())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            tracing::warn!(event="cache_write_failed", %key);
        }
    }
}
