FROM python:3.13-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends postgresql-client && rm -rf /var/lib/apt/lists/*
WORKDIR /tests
COPY tests/e2e.py ./
CMD ["python", "-u", "e2e.py"]
