# Marketplace: ДЗ 2

ДЗ 2 развивает каркас Orders из первого задания: Rust + Axum, контракт OpenAPI,
PostgreSQL, Flyway, CRUD товаров, заказы, промокоды, JWT и роли.

В ДЗ 2 Users, Catalog и Orders реализованы как модули одного приложения с одной
БД: задание требует атомарную транзакцию между заказом, остатками и промокодом.

## Запуск

Нужны Docker Engine и Docker Compose v2. Команды из корня репозитория:

```sh
docker compose up --build -d --wait --wait-timeout 180
curl -i http://localhost:8080/health
```

Сначала PostgreSQL становится healthy, затем **Flyway** применяет миграции,
после его успешного завершения запускается API. `/health` проверяет соединение с БД.
Первичная сборка загружает Rust-зависимости и может занять несколько минут.

- API: `http://localhost:8080`.
- Контракт: [services/orders/openapi/openapi.json](services/orders/openapi/openapi.json),
  также выдаётся через `GET /openapi.json`; его можно импортировать в Postman.
- Логи: `docker compose logs -f orders`.
- Проверка логов: `docker compose logs --no-log-prefix --no-color orders | python3 scripts/check-logs.py`.
- Миграции: `docker compose logs migrate`.
- Остановка с сохранением данных: `docker compose down`.

Настройки берутся из переменных окружения или `.env` (пример — `.env.example`):
`ORDERS_PORT=8080`, `ORDER_INTERVAL_MINUTES=1` (N, диапазон 0–1440),
`POSTGRES_PASSWORD`, `JWT_SECRET` (минимум 32 байта).
Значения секретов по умолчанию предназначены для локального учебного запуска.
Access token действует 20 минут, refresh token — 14 дней.

## Проверки и демонстрация

```sh
# Полные HTTP + PostgreSQL сценарии на запущенной системе:
docker compose --profile test run --build --rm e2e

# Демонстрация создания товара, промокода, заказа, обновления и отмены:
python3 scripts/demo.py

# SELECT обязателен на защите:
docker compose exec -T postgres psql -U marketplace -d marketplace -c \
  'SELECT id,name,price,stock,status,seller_id FROM products;'
docker compose exec -T postgres psql -U marketplace -d marketplace -c \
  'SELECT id,user_id,status,total_amount,discount_amount,promo_code_id FROM orders;'
docker compose exec -T postgres psql -U marketplace -d marketplace -c \
  'SELECT order_id,product_id,quantity,price_at_order FROM order_items;'
docker compose exec -T postgres psql -U marketplace -d marketplace -c \
  'SELECT code,current_uses,max_uses FROM promo_codes;'
```

E2E-тесты создают уникальные учебные аккаунты и товары. Используйте тестовую БД:
тесты назначают ADMIN своему аккаунту и сдвигают время своих операций через SQL,
чтобы проверить лимиты без ожидания. Таблицы и существующие данные не очищаются.
При тестировании оставьте `ORDER_INTERVAL_MINUTES=1`.

## Контракт и кодогенерация

```sh
sh scripts/generate-api.sh
# То же выполняется автоматически при обычной сборке:
cargo build --locked --manifest-path services/orders/Cargo.toml
cargo test --locked --manifest-path services/orders/Cargo.toml
cargo clippy --locked --manifest-path services/orders/Cargo.toml --all-targets -- -D warnings
```

Нужен Rust 1.91+ (Docker использует 1.95). `build.rs` — небольшой собственный
генератор поддерживаемого в проекте подмножества OpenAPI 3.0.3. Он создаёт **все
HTTP DTO и Axum-хэндлеры привязки маршрутов** в `target/.../out/api.rs`; результат
подключается через `include!` и исключён из Git. Это не OpenAPI Generator CLI.
OpenAPI — редактируемый источник истины; обратной генерации спецификации из кода нет.

`contract.rs` читает эту же спецификацию и до бизнес-логики проверяет required,
nullable, типы, enum, UUID/date-time/email, длины, диапазоны, multipleOf, pattern,
размеры массивов, path/query и дополнительные поля. Ошибки содержат
`details.violations` с путями полей. `api.rs` связывает сгенерированные маршруты
с реализацией; ручных HTTP DTO нет. JWT claims — внутренняя структура токена.

## API и роли

Для защищённых методов нужен `Authorization: Bearer <access_token>`.
USER/SELLER регистрируются через API; ADMIN назначается явно в БД, затем нужно
снова войти или обновить токены. Роль берётся из подписанного JWT, user_id и
seller_id нельзя передать в теле операции.

| Методы | Доступ и назначение |
| --- | --- |
| `POST /auth/register`, `/auth/login`, `/auth/refresh` | Публичные; refresh одноразовый, при использовании ротируется |
| `GET /products`, `GET /products/{id}` | USER, SELLER, ADMIN; включая архивные товары |
| `POST /products` | SELLER, ADMIN; владельцем становится текущий пользователь |
| `PUT /products/{id}`, `DELETE /products/{id}` | SELLER — свои товары, ADMIN — любые; DELETE архивирует |
| `POST /orders` | USER, ADMIN; заказ оформляется на текущего пользователя |
| `GET /orders/{id}`, `PUT /orders/{id}`, `POST /orders/{id}/cancel` | USER — свои заказы, ADMIN — любые |
| `POST /orders/{id}/status` | ADMIN; последовательное продвижение статуса для демонстрации |
| `POST /promo-codes` | SELLER, ADMIN |

Пагинация: `page=0`, `size=20` по умолчанию, максимум 100.
Фильтры `status` и `category` комбинируются, категория сравнивается точно.
Ответ: `items`, `totalElements`, `page`, `size`.
PUT товара заменяет все обязательные поля, отсутствие description очищает его.

## Бизнес-инварианты

- Порядок создания: лимит CREATE_ORDER → активный заказ → каталог → остатки →
  резервирование → снимки цен → промокод → сохранение и журнал операции.
- Один активный заказ (`CREATED`/`PAYMENT_PENDING`) на пользователя: блокировка
  строки пользователя и частичный уникальный индекс в PostgreSQL.
- Все изменения заказа, остатков, промокода и журнала выполняются в одной
  транзакции. Ошибка откатывает изменения. Товары блокируются `FOR UPDATE`
  в порядке UUID, промокод также блокируется перед проверкой его лимита.
- UPDATE: только CREATED; сначала владелец, затем статус и отдельный лимит
  UPDATE_ORDER. Старый резерв возвращается перед проверкой нового.
- Цена оставшейся позиции сохраняется из snapshot; новая позиция получает
  текущую цену. Изменение цены каталога не меняет существующий заказ.
- Повторение product_id в одном запросе запрещено: 400 с путём поля.
- Денежная арифметика — Decimal, БД — NUMERIC(12,2), JSON — number.
  Входные суммы кратны 0.01; скидка округляется до копеек, половина — вверх.
- PERCENTAGE ≤70 применяется; >70 означает нулевую скидку (полную цену),
  согласно фразе задания «в ином случае цена 100%». FIXED_AMOUNT не больше суммы.
- Обновление с промокодом не расходует его повторно: собственное использование
  вычитается при проверке лимита. Если сумма ниже минимума, связь и скидка
  снимаются, current_uses уменьшается. Истёкший/неактивный промокод даёт 422
  с откатом всего обновления.
- Отмена разрешена из CREATED/PAYMENT_PENDING. Она возвращает остатки и
  использование промокода один раз; повторная отмена даёт 409. Исторические
  суммы и ссылка на промокод сохраняются в отменённом заказе.
- Статусы: CREATED → PAYMENT_PENDING → PAID → SHIPPED → COMPLETED.
  Возвратов назад и пропуска состояний нет. Отмена — только через `/cancel`.

## Структура

| Файл/каталог | Назначение |
| --- | --- |
| `services/orders/openapi/openapi.json` | Контракт, DTO, ограничения, роли, каталог ошибок |
| `services/orders/build.rs` | Кодогенерация при каждой необходимой пересборке |
| `services/orders/src/contract.rs` | Валидация из контракта |
| `services/orders/src/api.rs` | Диспетчеризация и JSON-логи, X-Request-Id, маскирование секретов |
| `services/orders/src/auth.rs` | Argon2, JWT, ротация refresh |
| `services/orders/src/catalog.rs` | Товары и создание промокодов |
| `services/orders/src/orders.rs` | Транзакционные бизнес-операции |
| `services/orders/migrations` | Версионированные миграции Flyway |
| `tests/e2e.py` | HTTP + SQL проверки, включая конкурентные операции |

Логи каждого запроса содержат UUID request_id, method, endpoint, status_code,
duration_ms, user_id (или null), timestamp. Для POST/PUT/DELETE сохраняется
request_body с рекурсивным маскированием password и токенов. Некорректный JSON
не записывается целиком. X-Request-Id возвращается и на ошибках.
