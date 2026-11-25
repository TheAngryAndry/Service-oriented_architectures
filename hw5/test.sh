#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
export COMPOSE_PROJECT_NAME="soa-hw5-test-$$"
export GENERATE_EVENTS=false
# Isolated volumes and network; remove published ports to coexist with the demo.
override=$(mktemp)
python3 - <<'PY' > "$override"
import json
p=json.load(open('docker-compose.yml'))
p.pop('name',None)
for service in p['services'].values(): service.pop('ports',None)
print(json.dumps(p))
PY
compose=(docker compose --project-directory "$PWD" -f "$override")
cleanup() {
  code=$?
  if [ "$code" -ne 0 ]; then "${compose[@]}" logs --tail=80 >&2 || true; fi
  "${compose[@]}" down -v --remove-orphans
  rm -f "$override"
}
trap cleanup EXIT
"${compose[@]}" up --build -d --wait --wait-timeout 300
"${compose[@]}" --profile test run --build --rm tests
# Assert the exported document exists and has all mandatory metric names.
date=$(date -u -d '30 days ago' +%F)
"${compose[@]}" exec -T aggregation curl -fsS --aws-sigv4 aws:amz:us-east-1:s3 -u cinema:cinema-local-secret "http://s3:9000/movie-analytics/daily/$date/aggregates.json" | python3 -c 'import json,sys; x=json.load(sys.stdin); names={r["metric"] for r in x}; assert {"dau","conversion","avg_watch_seconds","retention_d1","retention_d7","movie_views"} <= names; assert all("computed_at" in r and "date" in r for r in x)'
