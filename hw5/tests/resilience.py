"""Fault and scheduler checks in test.sh's disposable Compose project only."""
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
import uuid

compose = sys.argv[1:]
assert compose and 'soa-hw5-test-' in os.environ['COMPOSE_PROJECT_NAME']


def run(*args):
    return subprocess.check_output([*compose, *args], text=True, timeout=150).strip()


def http(url, data=None, extra=()):
    args = ['exec', '-T', 'aggregation', 'curl', '-sS', '--max-time', '110', '-w', '\n%{http_code}']
    if data is not None:
        args += ['-H', 'Content-Type: application/json', '-d', json.dumps(data)]
    output = run(*args, *extra, url)
    body, code = output.rsplit('\n', 1)
    return int(code), body


def sql(query):
    code, body = http('http://clickhouse:8123/', extra=('-u', 'default:cinema-local', '--data-binary', query))
    assert code == 200, body
    return body.strip()


def eventually(check, seconds=100):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            if check():
                return
        except subprocess.CalledProcessError:
            pass
        time.sleep(2)
    raise AssertionError('condition did not become true: ' + str(check))


def metric(date, name):
    return sql(f"SELECT value FROM daily_metrics FINAL WHERE date='{date}' AND metric='{name}' AND dimension='' ")


def s3(date):
    return http(f'http://s3:9000/movie-analytics/daily/{date}/aggregates.json', extra=(
        '--header', 'x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
        '--aws-sigv4', 'aws:amz:us-east-1:s3', '-u', 'cinema:cinema-local-secret'))


# Metadata from Kafka itself, including live replica sets and effective min ISR.
description = run('exec', '-T', 'kafka-2', 'kafka-topics', '--bootstrap-server', 'kafka-2:9092', '--describe', '--topic', 'movie-events')
assert 'PartitionCount: 3' in description and 'ReplicationFactor: 2' in description, description
assert 'min.insync.replicas=1' in description, description
schema_code, schema = http('http://schema-registry:8081/config/movie-events-value')
assert schema_code == 200 and json.loads(schema)['compatibilityLevel'] == 'BACKWARD'
print('PASS: three partitions, replication=2, min ISR=1, BACKWARD schema', flush=True)

# Stop a broker that leads a movie-events partition, not an arbitrary idle node.
leader = re.search(r'Leader:\s*(\d+)', description).group(1)
broker = 'kafka-' + leader
run('stop', broker)
try:
    ids = []
    for n in range(12):
        event_id = str(uuid.uuid4())
        event = dict(event_id=event_id, user_id=f'failover-{n}', movie_id='failure-film',
                     event_type='VIEW_STARTED', timestamp=dt.datetime.now(dt.timezone.utc).isoformat(),
                     device_type='DESKTOP', session_id=event_id, progress_seconds=0)
        code, body = http('http://producer:8080/events', event)
        assert code == 201, (code, body)
        ids.append(event_id)
    values = ','.join("'" + x + "'" for x in ids)
    eventually(lambda: sql(f'SELECT count() FROM raw_events FINAL WHERE event_id IN ({values})') == '12')
    print(f'PASS: 12 API events reached ClickHouse with leader {broker} stopped', flush=True)
finally:
    run('start', broker)


def replicas_restored():
    text = run('exec', '-T', 'kafka-2', 'kafka-topics', '--bootstrap-server', 'kafka-2:9092', '--describe', '--topic', 'movie-events')
    isrs = re.findall(r'Isr:\s*([\d,]+)', text)
    return len(isrs) == 3 and all(len(i.split(',')) == 2 for i in isrs)


eventually(replicas_restored)
print('PASS: all partition replicas rejoined ISR after broker recovery', flush=True)

# Yesterday is exclusively maintained by the scheduler; no manual aggregate/export.
yesterday = (dt.datetime.now(dt.timezone.utc).date() - dt.timedelta(days=1)).isoformat()
eventually(lambda: s3(yesterday)[0] == 200)
old = s3(yesterday)[1]
run('stop', 's3')
try:
    code, body = http(f'http://aggregation:8080/export/{yesterday}', {})
    assert code == 503, (code, body)
finally:
    run('start', 's3')
eventually(lambda: s3(yesterday)[0] == 200 and s3(yesterday)[1] != old)
print('PASS: S3 outage returns 503; scheduler overwrites yesterday object after recovery', flush=True)

run('stop', 'postgres')
try:
    code, body = http(f'http://aggregation:8080/export/{yesterday}', {})
    assert code == 503, (code, body)
    eventually(lambda: 'PostgreSQL retry' in run('logs', '--no-color', 'aggregation'))
finally:
    run('start', 'postgres')
eventually(lambda: http(f'http://aggregation:8080/export/{yesterday}', {})[0] == 200)
print('PASS: PostgreSQL outage returns 503; export recovers', flush=True)

# A separate generator uses the same image and Kafka; no demo containers are touched.
generator = run('run', '--no-deps', '-d', '-e', 'GENERATE_EVENTS=true', 'producer')
try:
    eventually(lambda: sql("SELECT count() FROM raw_events FINAL WHERE startsWith(user_id,'demo-')") != '0')
    eventually(lambda: sql("SELECT count() FROM raw_events FINAL WHERE startsWith(user_id,'demo-') AND event_type='LIKED'") != '0')
    data = sql("SELECT session_id,event_type,progress_seconds FROM raw_events FINAL WHERE startsWith(user_id,'demo-') ORDER BY timestamp FORMAT JSONEachRow")
    sessions = {}
    for line in data.splitlines():
        row = json.loads(line)
        sessions.setdefault(row['session_id'], []).append(row)
    complete = [rows for rows in sessions.values() if rows[-1]['event_type'] == 'LIKED']
    assert complete
    for rows in complete:
        assert [r['event_type'] for r in rows] == ['SEARCHED','VIEW_STARTED','VIEW_PAUSED','VIEW_RESUMED','VIEW_FINISHED','LIKED']
        progress = [r['progress_seconds'] for r in rows]
        assert progress == sorted(progress) and progress[-1] > progress[1]
    today = dt.datetime.now(dt.timezone.utc).date().isoformat()
    eventually(lambda: float(metric(today, 'avg_watch_seconds') or 0) == 1800)
    print('PASS: generator ordering/progress and automatic scheduled metrics', flush=True)
finally:
    subprocess.run(['docker', 'rm', '-f', generator], check=True, stdout=subprocess.DEVNULL)

logs = run('logs', '--no-color', 'aggregation')
assert 'aggregation started' in logs and 'aggregation completed' in logs
assert 'processed' in logs and 'elapsed_ms' in logs
assert 'PostgreSQL retry' in logs and ('export failed' in logs or 'operation failed' in logs)
print('PASS: cycle counters, timing and failure/retry logs', flush=True)
