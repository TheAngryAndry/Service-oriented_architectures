"""Validate API JSON logs read from stdin, e.g. docker compose logs --no-log-prefix."""
import datetime
import json
import sys
import uuid

SECRET_KEYS = {"password", "refresh_token", "access_token", "authorization", "secret"}


def check_redaction(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if key.lower() in SECRET_KEYS:
                assert item == "***", f"Unmasked field: {key}"
            else:
                check_redaction(item)
    elif isinstance(value, list):
        for item in value:
            check_redaction(item)


count = 0
for line in sys.stdin:
    event = json.loads(line)
    if "request_id" not in event:
        continue  # Startup/SQL diagnostics are also JSON, but are not HTTP events.
    uuid.UUID(event["request_id"])
    datetime.datetime.fromisoformat(event["timestamp"])
    assert event["method"]
    assert event["endpoint"].startswith("/")
    assert 100 <= event["status_code"] <= 599
    assert event["duration_ms"] >= 0
    if event["user_id"] is not None:
        uuid.UUID(event["user_id"])
    if event["method"] in {"POST", "PUT", "DELETE"}:
        check_redaction(event["request_body"])
    count += 1
assert count > 0, "No HTTP events found"
print(f"PASS: {count} JSON API log records, required fields and secret masking")
