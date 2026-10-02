FROM python:3.13-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PORT=8080

WORKDIR /app
COPY --chown=10001:10001 services/orders/app.py /app/app.py

USER 10001:10001
EXPOSE 8080

HEALTHCHECK --interval=5s --timeout=3s --start-period=3s --retries=5 \
    CMD ["python", "-c", "import urllib.request; r = urllib.request.urlopen('http://127.0.0.1:8080/health', timeout=2); raise SystemExit(0 if r.status == 200 else 1)"]

CMD ["python", "app.py"]
