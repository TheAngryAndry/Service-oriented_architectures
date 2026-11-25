CREATE TABLE IF NOT EXISTS raw_events (
 event_id UUID, user_id String, movie_id String,
 event_type LowCardinality(String), timestamp DateTime64(3,'UTC'),
 device_type LowCardinality(String), session_id String, progress_seconds Int32
) ENGINE=ReplacingMergeTree PARTITION BY toYYYYMM(timestamp) ORDER BY (toDate(timestamp),user_id,event_id);
CREATE TABLE IF NOT EXISTS kafka_events (
 event_id String, user_id String, movie_id String, event_type String,
 timestamp DateTime64(3,'UTC'), device_type String, session_id String, progress_seconds Int32
) ENGINE=Kafka SETTINGS kafka_broker_list='kafka-1:9092,kafka-2:9092', kafka_topic_list='movie-events',
 kafka_group_name='clickhouse-movie-v1', kafka_format='AvroConfluent', kafka_num_consumers=3,
 format_avro_schema_registry_url='http://schema-registry:8081';
CREATE MATERIALIZED VIEW IF NOT EXISTS ingest_events TO raw_events AS
 SELECT toUUID(event_id) AS event_id,user_id,movie_id,event_type,timestamp,device_type,session_id,progress_seconds FROM kafka_events;
CREATE TABLE IF NOT EXISTS daily_metrics (
 date Date, metric String, dimension String, value Float64, computed_at DateTime64(3,'UTC')
) ENGINE=ReplacingMergeTree(computed_at) ORDER BY (date,metric,dimension);
