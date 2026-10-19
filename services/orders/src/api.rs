use crate::{
    AppState, auth, catalog, contract,
    error::{ApiError, Result},
    generated::*,
    orders,
};
use axum::{
    Json,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, time::Instant};
use uuid::Uuid;

const BODY_LIMIT: usize = 128 * 1024;
fn redact(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (k, v) in o {
                if [
                    "password",
                    "refresh_token",
                    "access_token",
                    "authorization",
                    "secret",
                ]
                .contains(&k.to_ascii_lowercase().as_str())
                {
                    *v = json!("***");
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                redact(v);
            }
        }
        _ => {}
    }
}
pub async fn log_request(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let start = Instant::now();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let request_id = Uuid::new_v4().to_string();
    let method = req.method().clone();
    let endpoint = req.uri().path().to_string();
    let user_id = auth::authenticate(&state, req.headers())
        .ok()
        .map(|c| c.sub);
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, BODY_LIMIT).await;
    let mut logged_body = Value::Null;
    let mut response = match bytes {
        Ok(bytes) => {
            if matches!(method, Method::POST | Method::PUT | Method::DELETE) {
                logged_body = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap_or(json!("[invalid JSON omitted]"))
                };
                redact(&mut logged_body);
            }
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(_) => ApiError::validation("body", "Максимальный размер тела: 128 KiB").into_response(),
    };
    response
        .headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());
    // Explicit JSON event keeps required nullable fields and body structured.
    println!(
        "{}",
        json!({"request_id":request_id,"method":method.as_str(),"endpoint":endpoint,
        "status_code":response.status().as_u16(),"duration_ms":start.elapsed().as_secs_f64()*1000.0,
        "user_id":user_id,"timestamp":timestamp,"request_body":logged_body})
    );
    response
}
fn reply<T: Serialize>(status: u16, value: T) -> Response {
    (StatusCode::from_u16(status).unwrap(), Json(value)).into_response()
}
pub async fn dispatch(state: AppState, req: Request, operation: &'static str) -> Response {
    match execute(&state, req, operation).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}
async fn execute(state: &AppState, req: Request, id: &str) -> Result<Response> {
    let op = contract::operation(id);
    let user = if let Some(roles) = op["x-roles"].as_array() {
        let user = auth::authenticate(state, req.headers())?;
        if !roles.iter().any(|r| r == &user.role) {
            return Err(ApiError::new(
                "ACCESS_DENIED",
                "Роль не допускает эту операцию",
            ));
        }
        Some(user)
    } else {
        None
    };
    let mut query: HashMap<String, Value> = HashMap::new();
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(req.uri().query().unwrap_or(""))
        .map_err(|_| ApiError::validation("query", "Некорректные query-параметры"))?;
    let mut path_id = None;
    if let Some(params) = op["parameters"].as_array() {
        for param in params {
            let name = param["name"].as_str().unwrap();
            if param["in"] == "path" {
                let v = req.uri().path().split('/').nth(2).unwrap_or("");
                contract::validate(&param["schema"], &json!(v), &format!("path.{name}"))?;
                path_id = Some(Uuid::parse_str(v).unwrap());
            } else {
                let values: Vec<_> = pairs.iter().filter(|(k, _)| k == name).collect();
                if values.len() > 1 {
                    return Err(ApiError::validation(
                        &format!("query.{name}"),
                        "Параметр повторяется",
                    ));
                }
                let schema = contract::resolve(&param["schema"]);
                let value = if let Some((_, v)) = values.first() {
                    if schema["type"] == "integer" {
                        json!(v.parse::<i64>().map_err(|_| ApiError::validation(
                            &format!("query.{name}"),
                            "Ожидается целое число"
                        ))?)
                    } else {
                        json!(v)
                    }
                } else if let Some(default) = schema.get("default") {
                    default.clone()
                } else {
                    continue;
                };
                contract::validate(schema, &value, &format!("query.{name}"))?;
                query.insert(name.into(), value);
            }
        }
    }
    for (name, _) in &pairs {
        if !query.contains_key(name) {
            return Err(ApiError::validation(
                &format!("query.{name}"),
                "Неизвестный query-параметр",
            ));
        }
    }
    let is_json = req
        .headers()
        .get("content-type")
        .and_then(|x| x.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|s| s.trim() == "application/json")
        });
    let bytes = to_bytes(req.into_body(), BODY_LIMIT)
        .await
        .map_err(|_| ApiError::validation("body", "Слишком большое тело"))?;
    let body = if let Some(request) = op.get("requestBody") {
        if !is_json {
            return Err(ApiError::validation(
                "Content-Type",
                "Требуется application/json",
            ));
        }
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::validation("body", "Некорректный JSON"))?;
        contract::validate(
            &request["content"]["application/json"]["schema"],
            &body,
            "body",
        )?;
        // Cross-field input constraints are checked before business logic.
        if let Some(items) = body["items"].as_array() {
            let mut seen = std::collections::HashSet::new();
            for (i, item) in items.iter().enumerate() {
                let product = Uuid::parse_str(item["product_id"].as_str().unwrap()).unwrap();
                if !seen.insert(product) {
                    return Err(ApiError::validation(
                        &format!("body.items[{i}].product_id"),
                        "Товар не должен повторяться",
                    ));
                }
            }
        }
        body
    } else {
        if !bytes.is_empty() {
            return Err(ApiError::validation(
                "body",
                "Тело не предусмотрено контрактом",
            ));
        }
        Value::Null
    };
    let result = match id {
        "health" => {
            sqlx::query("SELECT 1").execute(&state.db).await?;
            reply(
                200,
                HealthResponse {
                    status: "ok".into(),
                },
            )
        }
        "register" => reply(
            201,
            auth::register(state, serde_json::from_value(body)?).await?,
        ),
        "login" => reply(
            200,
            auth::login(state, serde_json::from_value(body)?).await?,
        ),
        "refresh" => reply(
            200,
            auth::refresh(state, serde_json::from_value(body)?).await?,
        ),
        "create_product" => reply(
            201,
            catalog::create(state, user.as_ref().unwrap(), serde_json::from_value(body)?).await?,
        ),
        "get_product" => reply(200, catalog::get(state, path_id.unwrap()).await?),
        "update_product" => reply(
            200,
            catalog::update(
                state,
                user.as_ref().unwrap(),
                path_id.unwrap(),
                serde_json::from_value(body)?,
            )
            .await?,
        ),
        "delete_product" => {
            catalog::archive(state, user.as_ref().unwrap(), path_id.unwrap()).await?;
            StatusCode::NO_CONTENT.into_response()
        }
        "list_products" => reply(
            200,
            catalog::list(
                state,
                query["page"].as_i64().unwrap() as i32,
                query["size"].as_i64().unwrap() as i32,
                query
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                query
                    .get("category")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
            .await?,
        ),
        "create_promo" => reply(
            201,
            catalog::create_promo(state, serde_json::from_value(body)?).await?,
        ),
        "create_order" => reply(
            201,
            orders::create(state, user.as_ref().unwrap(), serde_json::from_value(body)?).await?,
        ),
        "get_order" => reply(
            200,
            orders::get(state, user.as_ref().unwrap(), path_id.unwrap()).await?,
        ),
        "update_order" => reply(
            200,
            orders::update(
                state,
                user.as_ref().unwrap(),
                path_id.unwrap(),
                serde_json::from_value(body)?,
            )
            .await?,
        ),
        "cancel_order" => reply(
            200,
            orders::cancel(state, user.as_ref().unwrap(), path_id.unwrap()).await?,
        ),
        "transition_order" => reply(
            200,
            orders::transition(state, path_id.unwrap(), serde_json::from_value(body)?).await?,
        ),
        _ => return Err(ApiError::new("INTERNAL_ERROR", "Операция не реализована")),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_redacted_recursively() {
        let mut body =
            json!({"Password":"secret","nested":[{"refresh_token":"secret"}],"name":"visible"});
        redact(&mut body);
        assert_eq!(
            body,
            json!({"Password":"***","nested":[{"refresh_token":"***"}],"name":"visible"})
        );
    }
}
