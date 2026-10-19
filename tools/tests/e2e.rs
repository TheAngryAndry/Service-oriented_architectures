//! Real HTTP + PostgreSQL checks. Run with --ignored against a test database.
//! Fixtures use fresh UUIDs; tests never truncate existing data.
mod support;
use chrono::{DateTime, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde_json::{Value, json};
use std::env;
use support::*;
use uuid::Uuid;

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn products_crud_filter_and_validation() {
    let c = Context::new();
    let (token, _) = c.account("USER");
    let p = c.product(json!({}));
    assert_eq!(p["seller_id"], c.seller_id);
    assert_eq!(
        c.call(200, "GET", &path("products", &p), Value::Null, &token)["id"],
        p["id"]
    );
    let category = p["category"].as_str().unwrap();
    let listing = c.call(
        200,
        "GET",
        &format!("/products?category={category}&status=ACTIVE&size=1"),
        Value::Null,
        &token,
    );
    assert_eq!(
        (
            listing["totalElements"].as_i64(),
            listing["page"].as_i64(),
            listing["size"].as_i64(),
            listing["items"].as_array().unwrap().len()
        ),
        (Some(1), Some(0), Some(1), 1)
    );
    let listing = c.call(
        200,
        "GET",
        &format!("/products?category={category}&page=1&size=1"),
        Value::Null,
        &token,
    );
    assert_eq!(listing["totalElements"], 1);
    assert_eq!(listing["items"], json!([]));
    for query in [
        "page=-1",
        "size=0",
        "size=101",
        "status=BAD",
        "page=no",
        "size=1&size=2",
    ] {
        c.error(
            400,
            "VALIDATION_ERROR",
            "GET",
            &format!("/products?{query}"),
            Value::Null,
            &token,
        );
    }
    let error = c.error(
        400,
        "VALIDATION_ERROR",
        "POST",
        "/products",
        json!({"name":"","price":0,"stock":-1,"category":"","status":"BAD"}),
        &c.seller,
    );
    assert_eq!(error["details"]["violations"].as_array().unwrap().len(), 5);
    for patch in [
        json!({"name":"x".repeat(256)}),
        json!({"description":"x".repeat(4001)}),
        json!({"category":"x".repeat(101)}),
        json!({"price":0.001}),
        json!({"stock":1.5}),
        json!({"seller_id":c.admin_id}),
    ] {
        let body = json!({"name":p["name"],"description":p["description"],"price":100,"stock":p["stock"],"category":p["category"],"status":p["status"]});
        c.error(
            400,
            "VALIDATION_ERROR",
            "POST",
            "/products",
            merge(body, patch),
            &c.seller,
        );
    }
    let updated = c.call(
        200,
        "PUT",
        &path("products", &p),
        json!({"name":"Updated","price":125,"stock":10,"category":category,"status":"INACTIVE"}),
        &c.seller,
    );
    assert_eq!(updated["created_at"], p["created_at"]);
    assert!(
        DateTime::parse_from_rfc3339(updated["updated_at"].as_str().unwrap()).unwrap()
            > DateTime::parse_from_rfc3339(p["updated_at"].as_str().unwrap()).unwrap()
    );
    c.call(204, "DELETE", &path("products", &p), Value::Null, &c.seller);
    assert_eq!(
        c.sql(&format!(
            "SELECT status FROM products WHERE id='{}'",
            id(&p)
        )),
        "ARCHIVED"
    );
    c.error(
        404,
        "PRODUCT_NOT_FOUND",
        "GET",
        &format!("/products/{}", Uuid::new_v4()),
        Value::Null,
        &token,
    );
    c.error(
        400,
        "VALIDATION_ERROR",
        "GET",
        "/products/not-uuid",
        Value::Null,
        &token,
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn roles_and_ownership() {
    let c = Context::new();
    let (buyer, _) = c.account("USER");
    let (other, _) = c.account("USER");
    let (seller, _) = c.account("SELLER");
    let p = c.product(json!({}));
    c.error(403, "ACCESS_DENIED", "POST", "/products", json!({}), &buyer);
    c.error(
        403,
        "ACCESS_DENIED",
        "DELETE",
        &path("products", &p),
        Value::Null,
        &seller,
    );
    c.error(403, "ACCESS_DENIED", "POST", "/orders", json!({}), &seller);
    c.error(
        403,
        "ACCESS_DENIED",
        "POST",
        "/promo-codes",
        json!({}),
        &buyer,
    );
    c.error(
        400,
        "VALIDATION_ERROR",
        "POST",
        "/auth/register",
        json!({"email":"root@test.example","password":PASSWORD,"role":"ADMIN"}),
        "",
    );
    let order = c.order(&buyer, &p, 1, json!({}));
    let path = path("orders", &order);
    for (method, endpoint, body) in [
        ("GET", path.clone(), Value::Null),
        ("PUT", path.clone(), items(&p, 2)),
        ("POST", format!("{path}/cancel"), Value::Null),
    ] {
        c.error(
            403,
            "ORDER_OWNERSHIP_VIOLATION",
            method,
            &endpoint,
            body,
            &other,
        );
    }
    c.error(403, "ACCESS_DENIED", "GET", &path, Value::Null, &seller);
    c.call(200, "GET", &path, Value::Null, &c.admin);
    c.call(
        200,
        "POST",
        &format!("{path}/cancel"),
        Value::Null,
        &c.admin,
    );
    c.call(
        204,
        "DELETE",
        &support::path("products", &p),
        Value::Null,
        &c.admin,
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn atomic_create_and_shortage_details() {
    let c = Context::new();
    let (buyer, user) = c.account("USER");
    let p = c.product(json!({"stock":2}));
    let q = c.product(json!({"stock":0}));
    let error = c.error(
        409,
        "INSUFFICIENT_STOCK",
        "POST",
        "/orders",
        json!({"items":[{"product_id":p["id"],"quantity":3},{"product_id":q["id"],"quantity":1}]}),
        &buyer,
    );
    assert_eq!(error["details"]["products"].as_array().unwrap().len(), 2);
    c.error(
        422,
        "PROMO_CODE_INVALID",
        "POST",
        "/orders",
        merge(items(&p, 1), json!({"promo_code":"MISSING"})),
        &buyer,
    );
    assert_eq!(c.stock(&p), 2);
    assert_eq!(
        c.sql(&format!(
            "SELECT count(*) FROM orders WHERE user_id='{user}'"
        )),
        "0"
    );
    assert_eq!(
        c.sql(&format!(
            "SELECT count(*) FROM user_operations WHERE user_id='{user}'"
        )),
        "0"
    );
    c.error(
        404,
        "PRODUCT_NOT_FOUND",
        "POST",
        "/orders",
        items(&json!({"id":Uuid::new_v4()}), 1),
        &buyer,
    );
    let inactive = c.product(json!({"status":"INACTIVE"}));
    c.error(
        409,
        "PRODUCT_INACTIVE",
        "POST",
        "/orders",
        items(&inactive, 1),
        &buyer,
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn rate_order_priority_and_cancel() {
    let c = Context::new();
    let (buyer, user) = c.account("USER");
    let p = c.product(json!({"stock":5}));
    let promo = c.promo(json!({"max_uses":1}));
    let order = c.order(&buyer, &p, 2, json!({"promo_code":promo["code"]}));
    assert_eq!(money(&order["total_amount"]), 180.into());
    assert_eq!(money(&order["discount_amount"]), 20.into());
    assert_eq!(c.stock(&p), 3);
    c.error(
        429,
        "ORDER_LIMIT_EXCEEDED",
        "POST",
        "/orders",
        items(&p, 1),
        &buyer,
    );
    c.age(&user);
    c.error(
        409,
        "ORDER_HAS_ACTIVE",
        "POST",
        "/orders",
        items(&p, 1),
        &buyer,
    );
    let cancel = format!("{}/cancel", path("orders", &order));
    c.call(200, "POST", &cancel, Value::Null, &buyer);
    assert_eq!(c.stock(&p), 5);
    assert_eq!(
        c.sql(&format!(
            "SELECT current_uses FROM promo_codes WHERE id='{}'",
            id(&promo)
        )),
        "0"
    );
    c.error(
        409,
        "INVALID_STATE_TRANSITION",
        "POST",
        &cancel,
        Value::Null,
        &buyer,
    );
    assert_eq!(c.stock(&p), 5);
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn update_snapshot_discount_removal_and_rollback() {
    let c = Context::new();
    let (buyer, user) = c.account("USER");
    let p = c.product(json!({"stock":10}));
    let promo = c.promo(json!({"min_order_amount":200,"max_uses":1}));
    let order = c.order(&buyer, &p, 3, json!({"promo_code":promo["code"]}));
    let path = path("orders", &order);
    c.sql(&format!(
        "UPDATE products SET price=999 WHERE id='{}'",
        id(&p)
    ));
    let current = c.call(200, "GET", &path, Value::Null, &buyer);
    assert_eq!(money(&current["items"][0]["price_at_order"]), 100.into());
    let updated = c.call(200, "PUT", &path, items(&p, 2), &buyer);
    assert_eq!(money(&updated["total_amount"]), 180.into());
    assert_eq!(money(&updated["discount_amount"]), 20.into());
    assert_eq!(c.stock(&p), 8);
    c.error(
        429,
        "ORDER_LIMIT_EXCEEDED",
        "PUT",
        &path,
        items(&p, 1),
        &buyer,
    );
    c.age(&user);
    let updated = c.call(200, "PUT", &path, items(&p, 1), &buyer);
    assert_eq!(money(&updated["total_amount"]), 100.into());
    assert_eq!(money(&updated["discount_amount"]), 0.into());
    assert!(updated["promo_code_id"].is_null());
    assert_eq!(
        c.sql(&format!(
            "SELECT current_uses FROM promo_codes WHERE id='{}'",
            id(&promo)
        )),
        "0"
    );
    c.age(&user);
    c.error(
        409,
        "INSUFFICIENT_STOCK",
        "PUT",
        &path,
        items(&p, 11),
        &buyer,
    );
    assert_eq!(c.stock(&p), 9);
    assert_eq!(c.call(200, "GET", &path, Value::Null, &buyer), updated);
    assert_eq!(c.sql(&format!("SELECT count(*) FROM user_operations WHERE user_id='{user}' AND operation_type='UPDATE_ORDER'")),"2");
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn promo_validity_rounding_and_limits() {
    let c = Context::new();
    let p = c.product(json!({}));
    let (buyer, _) = c.account("USER");
    let expired =
        c.promo(json!({"valid_from":"2000-01-01T00:00:00Z","valid_until":"2001-01-01T00:00:00Z"}));
    let disabled = c.promo(json!({"active":false}));
    let future =
        c.promo(json!({"valid_from":"2098-01-01T00:00:00Z","valid_until":"2099-01-01T00:00:00Z"}));
    let minimum = c.promo(json!({"min_order_amount":101}));
    for (promo, code) in [
        (expired, "PROMO_CODE_INVALID"),
        (disabled, "PROMO_CODE_INVALID"),
        (future, "PROMO_CODE_INVALID"),
        (minimum, "PROMO_CODE_MIN_AMOUNT"),
    ] {
        c.error(
            422,
            code,
            "POST",
            "/orders",
            merge(items(&p, 1), json!({"promo_code":promo["code"]})),
            &buyer,
        );
    }
    for (value, kind, expected) in [
        (70, "PERCENTAGE", 30),
        (71, "PERCENTAGE", 100),
        (200, "FIXED_AMOUNT", 0),
    ] {
        let (buyer, _) = c.account("USER");
        let promo = c.promo(json!({"discount_value":value,"discount_type":kind}));
        assert_eq!(
            money(&c.order(&buyer, &p, 1, json!({"promo_code":promo["code"]}))["total_amount"]),
            expected.into()
        );
    }
    let penny = c.product(json!({"price":0.05}));
    let (buyer, _) = c.account("USER");
    let promo = c.promo(json!({"discount_value":10}));
    assert_eq!(
        money(&c.order(&buyer, &penny, 1, json!({"promo_code":promo["code"]}))["total_amount"]),
        "0.04".parse().unwrap()
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn invalid_promo_on_update_rolls_back() {
    let c = Context::new();
    let (buyer, user) = c.account("USER");
    let p = c.product(json!({}));
    let promo = c.promo(json!({}));
    let order = c.order(&buyer, &p, 1, json!({"promo_code":promo["code"]}));
    c.sql(&format!(
        "UPDATE promo_codes SET active=false WHERE id='{}'",
        id(&promo)
    ));
    let path = path("orders", &order);
    c.error(
        422,
        "PROMO_CODE_INVALID",
        "PUT",
        &path,
        items(&p, 2),
        &buyer,
    );
    assert_eq!(c.stock(&p), 19);
    assert_eq!(c.call(200, "GET", &path, Value::Null, &buyer), order);
    assert_eq!(c.sql(&format!("SELECT count(*) FROM user_operations WHERE user_id='{user}' AND operation_type='UPDATE_ORDER'")),"0");
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn state_machine() {
    let c = Context::new();
    let (buyer, _) = c.account("USER");
    let p = c.product(json!({}));
    let order = c.order(&buyer, &p, 1, json!({}));
    let path = path("orders", &order);
    let status = format!("{path}/status");
    c.error(
        409,
        "INVALID_STATE_TRANSITION",
        "POST",
        &status,
        json!({"status":"PAID"}),
        &c.admin,
    );
    c.error(
        403,
        "ACCESS_DENIED",
        "POST",
        &status,
        json!({"status":"PAYMENT_PENDING"}),
        &buyer,
    );
    c.call(
        200,
        "POST",
        &status,
        json!({"status":"PAYMENT_PENDING"}),
        &c.admin,
    );
    c.error(
        409,
        "INVALID_STATE_TRANSITION",
        "PUT",
        &path,
        items(&p, 2),
        &buyer,
    );
    for state in ["PAID", "SHIPPED", "COMPLETED"] {
        assert_eq!(
            c.call(200, "POST", &status, json!({"status":state}), &c.admin)["status"],
            state
        );
        c.error(
            409,
            "INVALID_STATE_TRANSITION",
            "POST",
            &format!("{path}/cancel"),
            Value::Null,
            &buyer,
        );
    }
    let (other, _) = c.account("USER");
    let order = c.order(&other, &p, 1, json!({}));
    let path = support::path("orders", &order);
    c.call(
        200,
        "POST",
        &format!("{path}/status"),
        json!({"status":"PAYMENT_PENDING"}),
        &c.admin,
    );
    c.call(200, "POST", &format!("{path}/cancel"), Value::Null, &other);
    c.error(
        404,
        "ORDER_NOT_FOUND",
        "GET",
        &format!("/orders/{}", Uuid::new_v4()),
        Value::Null,
        &buyer,
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn concurrent_stock_reservation() {
    let c = Context::new();
    let p = c.product(json!({"stock":1}));
    let buyers: Vec<_> = (0..4).map(|_| c.account("USER").0).collect();
    let results = c.concurrent("POST", "/orders", &items(&p, 1), &buyers);
    assert_eq!(statuses(&results), [201, 409, 409, 409]);
    assert_eq!(c.stock(&p), 0);
    assert_eq!(
        c.sql(&format!(
            "SELECT sum(quantity) FROM order_items WHERE product_id='{}'",
            id(&p)
        )),
        "1"
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn concurrent_active_order_promo_refresh_and_cancel() {
    let c = Context::new();
    let (buyer, _) = c.account("USER");
    let p = c.product(json!({"stock":10}));
    let results = c.concurrent(
        "POST",
        "/orders",
        &items(&p, 1),
        &[buyer.clone(), buyer.clone()],
    );
    assert_eq!(statuses(&results), [201, 429]);
    let order = &results.iter().find(|r| r.status == 201).unwrap().body;
    let results = c.concurrent(
        "POST",
        &format!("{}/cancel", path("orders", order)),
        &Value::Null,
        &[buyer.clone(), buyer],
    );
    assert_eq!(statuses(&results), [200, 409]);
    assert_eq!(c.stock(&p), 10);
    let promo = c.promo(json!({"max_uses":1}));
    let buyers: Vec<_> = (0..2).map(|_| c.account("USER").0).collect();
    let results = c.concurrent(
        "POST",
        "/orders",
        &merge(items(&p, 1), json!({"promo_code":promo["code"]})),
        &buyers,
    );
    assert_eq!(statuses(&results), [201, 422]);
    assert_eq!(c.stock(&p), 9);
    assert_eq!(
        c.sql(&format!(
            "SELECT current_uses FROM promo_codes WHERE id='{}'",
            id(&promo)
        )),
        "1"
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn auth_refresh_and_expired_tokens() {
    let c = Context::new();
    let email = format!("{}@test.example", Uuid::new_v4().simple());
    let credentials = json!({"email":email,"password":PASSWORD});
    let user = c.call(201, "POST", "/auth/register", credentials.clone(), "");
    assert_eq!(user["role"], "USER");
    c.error(
        409,
        "EMAIL_ALREADY_EXISTS",
        "POST",
        "/auth/register",
        json!({"email":email.to_uppercase(),"password":PASSWORD}),
        "",
    );
    c.error(
        401,
        "TOKEN_INVALID",
        "POST",
        "/auth/login",
        json!({"email":email,"password":"wrong-password"}),
        "",
    );
    let tokens = c.call(200, "POST", "/auth/login", credentials, "");
    assert_eq!(tokens["expires_in"], 1200);
    let access = tokens["access_token"].as_str().unwrap();
    let refresh = tokens["refresh_token"].as_str().unwrap();
    c.error(401, "TOKEN_INVALID", "GET", "/products", Value::Null, "");
    c.error(
        401,
        "TOKEN_INVALID",
        "GET",
        "/products",
        Value::Null,
        refresh,
    );
    c.error(
        401,
        "REFRESH_TOKEN_INVALID",
        "POST",
        "/auth/refresh",
        json!({"refresh_token":access}),
        "",
    );
    let results = c.concurrent(
        "POST",
        "/auth/refresh",
        &json!({"refresh_token":refresh}),
        &[String::new(), String::new()],
    );
    assert_eq!(statuses(&results), [200, 401]);
    let fresh = &results.iter().find(|r| r.status == 200).unwrap().body;
    c.call(
        200,
        "GET",
        "/products",
        Value::Null,
        fresh["access_token"].as_str().unwrap(),
    );
    c.error(
        401,
        "REFRESH_TOKEN_INVALID",
        "POST",
        "/auth/refresh",
        json!({"refresh_token":refresh}),
        "",
    );
    let secret = env::var("JWT_SECRET").expect("JWT_SECRET is required for expired-token checks");
    let mut validation = Validation::default();
    validation.set_audience(&["marketplace-api"]);
    validation.set_issuer(&["marketplace"]);
    let mut claims = decode::<Value>(
        access,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .unwrap()
    .claims;
    claims["exp"] = json!(Utc::now().timestamp() - 10);
    let expired = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap();
    c.error(
        401,
        "TOKEN_EXPIRED",
        "GET",
        "/products",
        Value::Null,
        &expired,
    );
    c.error(
        401,
        "TOKEN_INVALID",
        "GET",
        "/products",
        Value::Null,
        "invalid",
    );
}

#[test]
#[ignore = "requires running API and PostgreSQL"]
fn order_input_and_error_envelopes() {
    let c = Context::new();
    let (buyer, _) = c.account("USER");
    let p = c.product(json!({}));
    let item = json!({"product_id":p["id"],"quantity":1});
    for body in [
        json!({"items":[]}),
        json!({"items":vec![item.clone();51]}),
        json!({"items":[item,item]}),
        items(&p, 0),
        items(&p, 1000),
        json!({"items":[{"product_id":"bad","quantity":1}]}),
        merge(items(&p, 1), json!({"promo_code":"lowercase"})),
    ] {
        c.error(400, "VALIDATION_ERROR", "POST", "/orders", body, &buyer);
    }
    assert_eq!(
        c.api
            .raw(
                "POST",
                "/orders",
                &Value::Null,
                &buyer,
                Some("{\"password\":\"SECRET\",")
            )
            .unwrap()
            .status,
        400
    );
    c.error(404, "NOT_FOUND", "GET", "/unknown", Value::Null, "");
    c.error(
        405,
        "METHOD_NOT_ALLOWED",
        "PATCH",
        "/products",
        Value::Null,
        &buyer,
    );
    assert_eq!(c.stock(&p), 20);
}
