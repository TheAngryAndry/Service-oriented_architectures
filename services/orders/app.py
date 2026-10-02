"""Infrastructure-only Orders Service skeleton. No business endpoints."""

import os
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


class HealthHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if urlsplit(self.path).path == "/health":
            status, body = HTTPStatus.OK, b'{"status":"ok"}\n'
        else:
            status, body = HTTPStatus.NOT_FOUND, b'{"error":"not_found"}\n'
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    port = int(os.environ.get("PORT", "8080"))
    with ThreadingHTTPServer(("0.0.0.0", port), HealthHandler) as server:
        print(f"Orders Service listening on 0.0.0.0:{port}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass


if __name__ == "__main__":
    main()
