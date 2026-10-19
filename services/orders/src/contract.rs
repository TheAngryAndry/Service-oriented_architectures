//! Validation is driven by the same OpenAPI schemas that generate the DTOs.
use crate::error::{ApiError, Result};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use std::{str::FromStr, sync::OnceLock};

pub fn spec() -> &'static Value {
    static SPEC: OnceLock<Value> = OnceLock::new();
    SPEC.get_or_init(|| serde_json::from_str(include_str!("../openapi/openapi.json")).unwrap())
}
pub fn resolve(schema: &Value) -> &Value {
    if let Some(r) = schema["$ref"].as_str() {
        spec().pointer(&r[1..]).expect("valid schema reference")
    } else {
        schema
    }
}
pub fn operation(id: &str) -> &'static Value {
    spec()["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|v| v.as_object().unwrap().values())
        .find(|v| v["operationId"] == id)
        .expect("generated operation")
}
pub fn validate(schema: &Value, value: &Value, path: &str) -> Result<()> {
    let mut errors = vec![];
    inspect(schema, value, path, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(
            ApiError::new("VALIDATION_ERROR", "Некорректные входные данные")
                .details(json!({"violations": errors})),
        )
    }
}
fn inspect(schema: &Value, value: &Value, path: &str, errors: &mut Vec<Value>) {
    let s = resolve(schema);
    if value.is_null() && s["nullable"] == true {
        return;
    }
    let mut fail = |message: String| errors.push(json!({"field":path,"message":message}));
    if let Some(variants) = s["enum"].as_array()
        && !variants.contains(value)
    {
        fail(format!("Допустимые значения: {variants:?}"));
    }
    match s["type"].as_str() {
        Some("object") => {
            let Some(object) = value.as_object() else {
                fail("Ожидается объект".into());
                return;
            };
            if let Some(required) = s["required"].as_array() {
                for name in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(name) {
                        errors.push(
                            json!({"field":format!("{path}.{name}"),"message":"Обязательное поле"}),
                        );
                    }
                }
            }
            for (name, v) in object {
                if let Some(prop) = s["properties"].get(name) {
                    inspect(prop, v, &format!("{path}.{name}"), errors);
                } else if s["additionalProperties"] == false {
                    errors.push(
                        json!({"field":format!("{path}.{name}"),"message":"Неизвестное поле"}),
                    );
                }
            }
        }
        Some("array") => {
            let Some(a) = value.as_array() else {
                fail("Ожидается массив".into());
                return;
            };
            for (key, violated) in [
                (
                    "minItems",
                    s["minItems"].as_u64().is_some_and(|n| a.len() < n as usize),
                ),
                (
                    "maxItems",
                    s["maxItems"].as_u64().is_some_and(|n| a.len() > n as usize),
                ),
            ] {
                if violated {
                    fail(format!("{key}: {}", s[key]));
                }
            }
            for (i, v) in a.iter().enumerate() {
                inspect(&s["items"], v, &format!("{path}[{i}]"), errors);
            }
        }
        Some("string") => {
            let Some(t) = value.as_str() else {
                fail("Ожидается строка".into());
                return;
            };
            let len = t.chars().count() as u64;
            if s["minLength"].as_u64().is_some_and(|n| len < n) {
                fail(format!("minLength: {}", s["minLength"]));
            }
            if s["maxLength"].as_u64().is_some_and(|n| len > n) {
                fail(format!("maxLength: {}", s["maxLength"]));
            }
            if let Some(pattern) = s["pattern"].as_str()
                && !regex::Regex::new(pattern)
                    .expect("contract regex")
                    .is_match(t)
            {
                fail(format!("pattern: {pattern}"));
            }
            let valid = match s["format"].as_str() {
                Some("uuid") => uuid::Uuid::parse_str(t).is_ok(),
                Some("date-time") => chrono::DateTime::parse_from_rfc3339(t).is_ok(),
                Some("email") => regex::Regex::new(r"^[^\s@]+@[^\s@]+\.[^\s@]+$")
                    .unwrap()
                    .is_match(t),
                _ => true,
            };
            if !valid {
                fail(format!("Некорректный формат {}", s["format"]));
            }
        }
        Some("integer" | "number") => {
            if !value.is_number() || (s["type"] == "integer" && value.as_i64().is_none()) {
                fail("Ожидается число соответствующего типа".into());
                return;
            }
            let Ok(n) = Decimal::from_str(&value.to_string()) else {
                fail("Число вне допустимого диапазона".into());
                return;
            };
            for key in ["minimum", "maximum", "multipleOf"] {
                if let Some(bound) = s.get(key) {
                    let b = Decimal::from_str(&bound.to_string()).expect("decimal bound");
                    let bad = match key {
                        "minimum" => n < b,
                        "maximum" => n > b,
                        _ => n % b != Decimal::ZERO,
                    };
                    if bad {
                        fail(format!("{key}: {b}"));
                    }
                }
            }
        }
        Some("boolean") if !value.is_boolean() => fail("Ожидается boolean".into()),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_all_invalid_fields() {
        let err = validate(
            &spec()["components"]["schemas"]["ProductCreate"],
            &json!({"name":"", "price":0, "stock":-1, "category":"", "status":"BAD"}),
            "body",
        )
        .unwrap_err();
        assert_eq!(
            err.details.unwrap()["violations"].as_array().unwrap().len(),
            5
        );
    }
    #[test]
    fn nullable_description_and_money_boundaries() {
        let schema = &spec()["components"]["schemas"]["ProductCreate"];
        let mut v = json!({"name":"я", "description":null, "price":0.01,"stock":0,"category":"x","status":"ACTIVE"});
        assert!(validate(schema, &v, "body").is_ok());
        v["price"] = json!(0.011);
        assert!(validate(schema, &v, "body").is_err());
        v["price"] = json!("1.00");
        assert!(validate(schema, &v, "body").is_err());
    }
    #[test]
    fn order_boundaries_and_pattern() {
        let schema = &spec()["components"]["schemas"]["OrderCreate"];
        assert!(validate(schema, &json!({"items":[],"promo_code":"bad"}), "body").is_err());
        let item = json!({"product_id":uuid::Uuid::new_v4(),"quantity":999});
        assert!(
            validate(
                schema,
                &json!({"items":vec![item.clone();50],"promo_code":"GOOD_1"}),
                "body"
            )
            .is_ok()
        );
        assert!(validate(schema, &json!({"items":vec![item;51]}), "body").is_err());
    }
}
