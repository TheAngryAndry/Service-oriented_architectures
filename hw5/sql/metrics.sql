WITH events AS (SELECT * FROM raw_events FINAL),
day AS (SELECT * FROM events WHERE toDate(timestamp)=toDate('{date}')),
cohorts AS (SELECT user_id,min(toDate(timestamp)) AS cohort FROM events WHERE event_type='VIEW_STARTED' GROUP BY user_id),
activity AS (SELECT DISTINCT user_id,toDate(timestamp) AS active FROM events WHERE event_type IN ('VIEW_STARTED','VIEW_FINISHED','VIEW_PAUSED','VIEW_RESUMED')),
retention AS (
 SELECT toUInt8(day_number) AS age,uniqExact(c.user_id) AS total,
 uniqExactIf(c.user_id,a.active=addDays(c.cohort,day_number)) AS returned
 FROM cohorts c CROSS JOIN (SELECT number AS day_number FROM numbers(8)) AS days
 LEFT JOIN activity a ON c.user_id=a.user_id
 WHERE c.cohort=toDate('{date}') GROUP BY age
)
SELECT 'dau' AS metric,'' AS dimension,toFloat64(uniqExact(user_id)) AS value FROM day
UNION ALL SELECT 'avg_watch_seconds','',if(countIf(event_type='VIEW_FINISHED')=0,0,avgIf(toFloat64(progress_seconds),event_type='VIEW_FINISHED')) FROM day
UNION ALL SELECT 'conversion','',if(countIf(event_type='VIEW_STARTED')=0,0,toFloat64(countIf(event_type='VIEW_FINISHED'))/countIf(event_type='VIEW_STARTED')) FROM day
UNION ALL SELECT 'events_processed','',toFloat64(count()) FROM day
UNION ALL SELECT 'movie_views',movie_id,toFloat64(countIf(event_type='VIEW_STARTED')) FROM day GROUP BY movie_id
UNION ALL SELECT concat('retention_d',toString(n.number)),'',if(r.total=0,0,toFloat64(r.returned)/r.total) FROM numbers(8) n LEFT JOIN retention r ON n.number=r.age WHERE addDays(toDate('{date}'),n.number)<=today()
FORMAT JSONEachRow
