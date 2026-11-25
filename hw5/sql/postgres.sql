CREATE TABLE IF NOT EXISTS daily_metrics (
 date date NOT NULL, metric text NOT NULL, dimension text NOT NULL DEFAULT '',
 value double precision NOT NULL, computed_at timestamptz NOT NULL,
 PRIMARY KEY(date,metric,dimension)
);
