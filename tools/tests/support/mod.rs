use chrono::{Duration, Utc};
use marketplace_tools::{Api, Response};
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{
    env,
    str::FromStr,
    sync::{Arc, Barrier},
    thread,
};
use tokio::runtime::Runtime;
use uuid::Uuid;

pub const PASSWORD: &str = "test-password-123";
pub fn id(value: &Value) -> &str {
    value["id"].as_str().unwrap()
}
pub fn path(resource: &str, value: &Value) -> String {
    format!("/{resource}/{}", id(value))
}
pub fn items(product: &Value, quantity: i64) -> Value {
    json!({"items":[{"product_id":product["id"],"quantity":quantity}]})
}
pub fn money(value: &Value) -> rust_decimal::Decimal {
    value.to_string().parse().unwrap()
}
pub fn merge(mut value: Value, patch: Value) -> Value {
    value
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    value
}

pub struct Context {
    pub api: Api,
    runtime: Runtime,
    pool: sqlx::PgPool,
    pub seller: String,
    pub seller_id: String,
    pub admin: String,
    pub admin_id: String,
}

impl Context {
    pub fn new() -> Self {
        let runtime = Runtime::new().unwrap();
        let mut options = match env::var("DATABASE_URL") {
            Ok(url) => PgConnectOptions::from_str(&url).unwrap(),
            Err(_) => PgConnectOptions::new(), // Supports PGHOST/PGPORT/PGUSER/PGDATABASE/PGPASSWORD.
        };
        options = options.application_name("marketplace-rust-e2e");
        let pool = runtime
            .block_on(
                PgPoolOptions::new()
                    .max_connections(2)
                    .connect_with(options),
            )
            .unwrap();
        let mut context = Self {
            api: Api::new().unwrap(),
            runtime,
            pool,
            seller: String::new(),
            seller_id: String::new(),
            admin: String::new(),
            admin_id: String::new(),
        };
        (context.seller, context.seller_id) = context.account("SELLER");
        (context.admin, context.admin_id) = context.account("ADMIN");
        context
    }
    pub fn sql(&self, statement: &str) -> String {
        if statement.starts_with("SELECT ") {
            let text: String = self
                .runtime
                .block_on(
                    sqlx::query_scalar(&format!(
                        "SELECT row_to_json(t)::text FROM ({statement}) t"
                    ))
                    .fetch_one(&self.pool),
                )
                .unwrap();
            let row: Value = serde_json::from_str(&text).unwrap();
            let value = row.as_object().unwrap().values().next().unwrap();
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        } else {
            self.runtime
                .block_on(sqlx::query(statement).execute(&self.pool))
                .unwrap();
            String::new()
        }
    }
    pub fn call(&self, status: u16, method: &str, path: &str, body: Value, token: &str) -> Value {
        let response = self.api.request(method, path, &body, token).unwrap();
        let mut visible = response.body.clone();
        marketplace_tools::redact(&mut visible);
        assert_eq!(response.status, status, "{method} {path}: {visible}");
        response.body
    }
    pub fn error(
        &self,
        status: u16,
        code: &str,
        method: &str,
        path: &str,
        body: Value,
        token: &str,
    ) -> Value {
        let result = self.call(status, method, path, body, token);
        assert_eq!(result["error_code"], code);
        result
    }
    pub fn account(&self, role: &str) -> (String, String) {
        let credentials =
            json!({"email":format!("{}@test.example",Uuid::new_v4().simple()),"password":PASSWORD});
        let user = self.call(
            201,
            "POST",
            "/auth/register",
            merge(
                credentials.clone(),
                json!({"role":if role=="ADMIN" {"USER"} else {role}}),
            ),
            "",
        );
        Uuid::parse_str(id(&user)).unwrap();
        if role == "ADMIN" {
            self.sql(&format!(
                "UPDATE users SET role='ADMIN' WHERE id='{}'",
                id(&user)
            ));
        }
        let tokens = self.call(200, "POST", "/auth/login", credentials, "");
        (
            tokens["access_token"].as_str().unwrap().into(),
            id(&user).into(),
        )
    }
    pub fn product(&self, patch: Value) -> Value {
        self.call(201,"POST","/products",merge(json!({"name":"Book","description":null,"price":100,"stock":20,"category":Uuid::new_v4().simple().to_string(),"status":"ACTIVE"}),patch),&self.seller)
    }
    pub fn promo(&self, patch: Value) -> Value {
        let now = Utc::now();
        self.call(201,"POST","/promo-codes",merge(json!({"code":format!("P_{}",&Uuid::new_v4().simple().to_string()[..12].to_uppercase()),"discount_type":"PERCENTAGE","discount_value":10,"min_order_amount":0,"max_uses":10,"active":true,"valid_from":(now-Duration::days(1)).to_rfc3339(),"valid_until":(now+Duration::days(1)).to_rfc3339()}),patch),&self.seller)
    }
    pub fn order(&self, token: &str, product: &Value, quantity: i64, extra: Value) -> Value {
        self.call(
            201,
            "POST",
            "/orders",
            merge(items(product, quantity), extra),
            token,
        )
    }
    pub fn stock(&self, product: &Value) -> i64 {
        self.sql(&format!(
            "SELECT stock FROM products WHERE id='{}'",
            id(product)
        ))
        .parse()
        .unwrap()
    }
    pub fn age(&self, user: &str) {
        self.sql(&format!(
            "UPDATE user_operations SET created_at=now()-interval '2 days' WHERE user_id='{user}'"
        ));
    }
    pub fn concurrent(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        tokens: &[String],
    ) -> Vec<Response> {
        let barrier = Arc::new(Barrier::new(tokens.len()));
        thread::scope(|scope| {
            let handles: Vec<_> = tokens
                .iter()
                .map(|token| {
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        self.api.request(method, path, body, token).unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        self.runtime.block_on(self.pool.close());
    }
}

pub fn statuses(results: &[Response]) -> Vec<u16> {
    let mut statuses: Vec<_> = results.iter().map(|r| r.status).collect();
    statuses.sort_unstable();
    statuses
}
