use chrono::DateTime;
use reqwest::{Method, blocking::Client};
use serde_json::{Value, json};
use std::{env, error::Error, io::BufRead, time::Duration};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub fn require(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned().into())
    }
}

pub struct Api {
    client: Client,
    base: String,
    spec: Value,
}

pub struct Response {
    pub status: u16,
    pub body: Value,
    pub request_id: String,
}

impl Api {
    pub fn new() -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let base = env::var("API_URL").unwrap_or_else(|_| "http://localhost:8080".into());
        let base = base.trim_end_matches('/').to_owned();
        let spec = client
            .get(format!("{base}/openapi.json"))
            .send()?
            .error_for_status()?
            .json()?;
        Ok(Self { client, base, spec })
    }

    pub fn request(&self, method: &str, path: &str, body: &Value, token: &str) -> Result<Response> {
        self.raw(method, path, body, token, None)
    }

    pub fn raw(
        &self,
        method: &str,
        path: &str,
        body: &Value,
        token: &str,
        raw: Option<&str>,
    ) -> Result<Response> {
        let mut request = self.client.request(
            Method::from_bytes(method.as_bytes())?,
            format!("{}{path}", self.base),
        );
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        if let Some(raw) = raw {
            request = request
                .header("Content-Type", "application/json")
                .body(raw.to_owned());
        } else if !body.is_null() {
            request = request.json(body);
        }
        let response = request.send()?;
        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("X-Request-Id")
            .ok_or("Missing X-Request-Id")?
            .to_str()?
            .to_owned();
        Uuid::parse_str(&request_id)?;
        let bytes = response.bytes()?;
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        if status >= 400 {
            let code = body["error_code"].as_str().ok_or("Missing error_code")?;
            require(
                self.spec["x-error-statuses"][code].as_u64() == Some(status as u64),
                "Error code/status violates OpenAPI",
            )?;
            require(body["message"].is_string(), "Missing error message")?;
        }
        let path_only = path.split('?').next().unwrap();
        for (template, methods) in self.spec["paths"]
            .as_object()
            .ok_or("Missing OpenAPI paths")?
        {
            let parts: Vec<_> = path_only.split('/').collect();
            let expected: Vec<_> = template.split('/').collect();
            if parts.len() != expected.len()
                || !parts
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| a == b || *b == "{id}")
            {
                continue;
            }
            if let Some(operation) = methods.get(method.to_lowercase()) {
                let schema = operation["responses"]
                    .get(status.to_string())
                    .ok_or("Undocumented response status")?;
                let schema = resolve(&self.spec, schema)?;
                if let Some(schema) = schema.pointer("/content/application~1json/schema") {
                    validate_response(&self.spec, schema, &body)?;
                } else {
                    require(bytes.is_empty(), "Expected empty response")?;
                }
            }
            break;
        }
        Ok(Response {
            status,
            body,
            request_id,
        })
    }
}

fn resolve<'a>(spec: &'a Value, schema: &'a Value) -> Result<&'a Value> {
    if let Some(reference) = schema["$ref"].as_str() {
        spec.pointer(
            reference
                .strip_prefix('#')
                .ok_or("External OpenAPI reference")?,
        )
        .ok_or_else(|| "Missing OpenAPI reference".into())
    } else {
        Ok(schema)
    }
}

// Independent response checks against the published contract, not server validation code.
pub fn validate_response(spec: &Value, schema: &Value, value: &Value) -> Result<()> {
    let schema = resolve(spec, schema)?;
    if value.is_null() && schema["nullable"] == true {
        return Ok(());
    }
    match schema["type"].as_str().ok_or("Missing schema type")? {
        "object" => {
            let object = value.as_object().ok_or("Expected object")?;
            if let Some(required) = schema["required"].as_array() {
                for key in required {
                    require(
                        object.contains_key(key.as_str().ok_or("Invalid required field")?),
                        "Missing required response field",
                    )?;
                }
            }
            for (key, item) in object {
                if let Some(property) = schema["properties"].get(key) {
                    validate_response(spec, property, item)?;
                } else {
                    require(
                        schema["additionalProperties"] != false,
                        "Unexpected response field",
                    )?;
                }
            }
        }
        "array" => {
            for item in value.as_array().ok_or("Expected array")? {
                validate_response(spec, &schema["items"], item)?;
            }
        }
        "number" => require(value.is_number(), "Expected number")?,
        "integer" => require(value.is_i64() || value.is_u64(), "Expected integer")?,
        "boolean" => require(value.is_boolean(), "Expected boolean")?,
        "string" => {
            let text = value.as_str().ok_or("Expected string")?;
            match schema["format"].as_str() {
                Some("uuid") => {
                    Uuid::parse_str(text)?;
                }
                Some("date-time") => {
                    DateTime::parse_from_rfc3339(text)?;
                }
                _ => {}
            }
        }
        _ => return Err("Unsupported response schema".into()),
    }
    if let Some(choices) = schema["enum"].as_array() {
        require(choices.contains(value), "Unknown enum value")?;
    }
    Ok(())
}

const SECRET_KEYS: [&str; 5] = [
    "password",
    "refresh_token",
    "access_token",
    "authorization",
    "secret",
];

pub fn redact(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, item) in object {
                if SECRET_KEYS.contains(&key.to_lowercase().as_str()) {
                    *item = json!("***");
                } else {
                    redact(item);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact(item);
            }
        }
        _ => {}
    }
}

pub fn check_logs(reader: impl BufRead) -> Result<usize> {
    let mut count = 0;
    for (line_number, line) in reader.lines().enumerate() {
        let event: Value = serde_json::from_str(&line?)
            .map_err(|_| format!("Invalid JSON on line {}", line_number + 1))?;
        if event.get("request_id").is_none() {
            continue;
        }
        Uuid::parse_str(event["request_id"].as_str().ok_or("Missing request_id")?)?;
        DateTime::parse_from_rfc3339(event["timestamp"].as_str().ok_or("Missing timestamp")?)?;
        let method = event["method"].as_str().ok_or("Missing method")?;
        require(!method.is_empty(), "Empty method")?;
        require(
            event["endpoint"]
                .as_str()
                .is_some_and(|p| p.starts_with('/')),
            "Invalid endpoint",
        )?;
        require(
            event["status_code"]
                .as_u64()
                .is_some_and(|s| (100..=599).contains(&s)),
            "Invalid status_code",
        )?;
        require(
            event["duration_ms"].as_f64().is_some_and(|v| v >= 0.),
            "Invalid duration_ms",
        )?;
        let user = event.get("user_id").ok_or("Missing user_id")?;
        if !user.is_null() {
            Uuid::parse_str(user.as_str().ok_or("Invalid user_id")?)?;
        }
        if ["POST", "PUT", "DELETE"].contains(&method) {
            let body = event.get("request_body").ok_or("Missing request_body")?;
            let mut masked = body.clone();
            redact(&mut masked);
            require(masked == *body, "Unmasked secret in request body")?;
        }
        count += 1;
    }
    require(count > 0, "No HTTP events found")?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn log_checker_rejects_missing_fields_and_nested_secrets() {
        let event = json!({"request_id":Uuid::new_v4(),"timestamp":"2026-10-19T12:00:00Z","method":"POST","endpoint":"/auth/login","status_code":200,"duration_ms":1,"user_id":null,"request_body":{"nested":[{"Password":"***"}]}});
        assert_eq!(check_logs(Cursor::new(event.to_string())).unwrap(), 1);
        let mut bad = event.clone();
        bad["request_body"]["nested"][0]["Password"] = json!("sensitive");
        assert!(check_logs(Cursor::new(bad.to_string())).is_err());
        let mut bad = event.clone();
        bad.as_object_mut().unwrap().remove("user_id");
        assert!(check_logs(Cursor::new(bad.to_string())).is_err());
        assert!(check_logs(Cursor::new("{}")).is_err());
        assert!(check_logs(Cursor::new("not json")).is_err());
        let mut bad = event;
        bad["status_code"] = json!(999);
        assert!(check_logs(Cursor::new(bad.to_string())).is_err());
    }

    #[test]
    fn response_validator_checks_refs_types_and_required_fields() {
        let schema = json!({"type":"object","required":["id","amount"],"additionalProperties":false,"properties":{"id":{"type":"string","format":"uuid"},"amount":{"type":"number"},"state":{"type":"string","enum":["CREATED"]}}});
        let spec = json!({"components":{"schemas":{"Example":schema}}});
        let reference = json!({"$ref":"#/components/schemas/Example"});
        let value = json!({"id":Uuid::new_v4(),"amount":0.04,"state":"CREATED"});
        assert!(validate_response(&spec, &reference, &value).is_ok());
        for patch in [
            json!({"amount":true}),
            json!({"id":"bad"}),
            json!({"state":"OTHER"}),
            json!({"extra":1}),
        ] {
            let mut bad = value.clone();
            bad.as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(validate_response(&spec, &reference, &bad).is_err());
        }
        assert!(validate_response(&spec, &reference, &json!({})).is_err());
    }
}
