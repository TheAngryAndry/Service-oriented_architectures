#!/bin/sh
set -eu
base_url="${API_URL:-http://localhost:8080}"
curl --fail --silent --show-error "$base_url/health"
printf '\n'
curl --fail --silent --show-error "$base_url/openapi.json" >/dev/null
printf 'PASS: health and OpenAPI\n'
