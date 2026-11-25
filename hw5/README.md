# ДЗ5 — Online Cinema Analytics (Rust)

Movie Service → Kafka (Avro + Schema Registry) → ClickHouse Kafka Engine →
ReplacingMergeTree. Отдельный Aggregation Service читает ClickHouse, сохраняет
метрики в ClickHouse и PostgreSQL, экспортирует PostgreSQL → RustFS. Grafana читает
материализованные агрегаты ClickHouse.

## Запуск

Из `hw5/`, Docker Engine + Compose v2:

```sh
docker compose up --build -d --wait --wait-timeout 300
```

Миграции, topic, бакет, Schema Registry, datasource и dashboard создаются автоматически.
Сборка Rust может занять несколько минут. Все пароли демонстрационные, порты привязаны к localhost.

- Producer: http://localhost:8085 (POST /events, GET /health).
- Aggregation: http://localhost:8086 (POST /aggregate/YYYY-MM-DD, POST /export/YYYY-MM-DD).
- ClickHouse HTTP: http://localhost:8125 (`default` / `cinema-local`).
- Grafana: http://localhost:3005 (`admin` / `cinema-local-admin`), dashboard Online Cinema Analytics.
- RustFS Console: http://localhost:9006 (`cinema` / `cinema-local-secret`).

```sh
curl -i http://localhost:8085/events -H 'Content-Type: application/json' -d '{
  "event_id":"ed25311f-2e91-42af-932e-098620c6ba1b",
  "user_id":"alice","movie_id":"movie-1","event_type":"VIEW_STARTED",
  "timestamp":"2026-11-25T12:00:00Z","device_type":"TV",
  "session_id":"session-1","progress_seconds":0
}'
curl -X POST http://localhost:8086/aggregate/2026-11-25
curl -X POST http://localhost:8086/export/2026-11-25
```

Успешная публикация — 201 с event_id, неверные бизнес-поля — 400,
неверный JSON/тип — 422, недоступная Kafka — 503. Генератор включён по умолчанию:
поиск → начало → пауза → продолжение → завершение → лайк. При ошибке публикации
последовательность прекращается, чтобы не публиковать завершение без начала.
`GENERATE_EVENTS=false docker compose up --build` отключает генератор.
Временные метки генератора отражают время отправки; progress имитирует ускоренное
воспроизведение фильма для демонстрации.

## Схема и доставка

`schemas/movie-event-v1.avsc` — v1; при старте продюсер регистрирует её под
subject `movie-events-value`, совместимость BACKWARD. Повторная регистрация той же
схемы возвращает существующий ID. Wire format: magic byte 0 + big-endian schema ID
+ Avro datum. timestamp — timestamp-millis UTC, event_id проверяется как UUID.
Enums, размеры идентификаторов и неотрицательный progress проверяются до Kafka.

Ключ — user_id: порядок событий пользователя сохраняется в одной из 3 партиций,
даже если он переключает фильмы. Три KRaft broker/controller позволяют сохранять
кворум при потере одного узла; replication.factor=2, min.insync.replicas=1,
acks=all, idempotent producer, exponential retry 100–5000 ms, timeout 30 s.
min ISR=1 повышает доступность, но потеря последней актуальной реплики до
восстановления второй может привести к потере данных.

ClickHouse хранит события по месяцам, сортировка (день,user_id,event_id).
ReplacingMergeTree + FINAL в аналитических запросах устраняют повторную доставку
одинакового события. Повторы должны иметь тот же event_id и неизменные поля;
это контракт идемпотентности, а не произвольное редактирование события.
Kafka ingestion использует at-least-once доставку. Невалидные записи стороннего
продюсера не пропускаются молча: ошибка видна в логах ClickHouse.

## Метрики

День — UTC. DAU = uniqExact пользователей всех типов событий. Среднее время —
avg(progress_seconds) завершённых просмотров. Топ — число VIEW_STARTED по фильму.
Конверсия — VIEW_FINISHED / VIEW_STARTED за день (0 при отсутствии стартов);
из-за завершений с предыдущего дня может быть >1, это отношение событий,
не сессионная воронка. Сервис также записывает events_processed.

Когорта определяется первым VIEW_STARTED за всю доступную историю.
Retention D0–D7 — доля пользователей когорты с событием просмотра ровно через
N календарных дней; SEARCHED и LIKED не считаются возвратом к просмотру.
Незрелые дни не сохраняются и показаны пустыми в таблице Grafana.
SQL использует uniqExact, avgIf, countIf, min и соединение когорт с активностью.

Расписание задаётся `AGGREGATION_INTERVAL_SECONDS` (300 по умолчанию):
пересчёт текущего дня и последних семи когорт, затем экспорт вчерашнего дня.
Поэтому вчерашний объект обновляется в течение суток; ключ всегда один:
`s3://movie-analytics/daily/YYYY-MM-DD/aggregates.json`.
Для поздних событий старше семи дней используйте ручной пересчёт и экспорт.
Исторический архив — снимок на момент экспорта; для обновления позднего retention
старой когорты нужно повторно вызвать export за её дату.

PostgreSQL: ключ (date,metric,dimension), транзакционный UPSERT, 3 попытки с backoff.
ClickHouse: ReplacingMergeTree по тому же ключу, версия computed_at.
Между БД нет распределённой транзакции: при сбое следующий пересчёт приводит
значения к одному состоянию. Экспорт читает только PostgreSQL, записывает JSON
с датой, именем, измерением, значением и computed_at. S3-ошибка логируется,
планировщик повторяет попытку в следующем цикле. HTTP-команды возвращают 503.

## Автоматические тесты

```sh
./test.sh
```

Нужны Bash, Python 3, GNU date и доступ к Docker (при необходимости `sudo ./test.sh`).
Runner создаёт отдельный Compose project, отключает генератор и публикацию портов,
поднимает весь стек, запускает Rust unit/e2e и проверяет файл в S3, затем удаляет
только свои контейнеры и volumes. UUID изолируют события; тест метрик использует
чистую БД тестового проекта. Ошибки возвращают ненулевой exit code.

Тесты проверяют все поля события через API → Kafka → ClickHouse, дедупликацию,
валидацию, DAU, среднее время, конверсию, D1/D7, повторный UPSERT, материализацию,
повторный экспорт и содержимое объекта, доступность datasource Grafana и SQL всех
панелей provisioned dashboard через API Grafana. Дополнительно `tests/resilience.py`
останавливает лидера Kafka, проверяет доставку и восстановление ISR, отказы S3/PG,
автоматическое расписание и последовательности генератора. Файлы: `tests/e2e.rs`, unit в `src/lib.rs`.

Остановка демо: `docker compose down`. Удаление демо-данных: `docker compose down -v`.

S3 экспорт использует curl с AWS Signature V4; endpoint и credentials задаются
через S3_ENDPOINT, S3_ACCESS_KEY, S3_SECRET_KEY. RustFS запущен в локальном
демонстрационном режиме с одним диском; это не отказоустойчивое S3-хранилище.

Сверка всех критериев и сценарий защиты: [VERIFICATION.md](VERIFICATION.md).
