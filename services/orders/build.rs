//! Small contract compiler for the OpenAPI subset used by this service.
//! No network, templates, manually maintained DTOs or generated files in Git.
use serde_json::Value;
use std::{env, fs, path::PathBuf};

fn rust_type(s: &Value) -> String {
    if let Some(r) = s["$ref"].as_str() {
        return r.rsplit('/').next().unwrap().into();
    }
    match (s["type"].as_str().unwrap(), s["format"].as_str()) {
        ("string", Some("uuid")) => "uuid::Uuid".into(),
        ("string", Some("date-time")) => "chrono::DateTime<chrono::Utc>".into(),
        ("string", _) => "String".into(),
        ("number", Some("decimal")) => "rust_decimal::Decimal".into(),
        ("integer", Some("int64")) => "i64".into(),
        ("integer", _) => "i32".into(),
        ("boolean", _) => "bool".into(),
        ("array", _) => format!("Vec<{}>", rust_type(&s["items"])),
        ("object", _) => "serde_json::Value".into(),
        other => panic!("Unsupported schema: {other:?}"),
    }
}

fn main() {
    let file = "openapi/openapi.json";
    println!("cargo:rerun-if-changed={file}");
    let spec: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let mut out = String::from("// Generated from openapi/openapi.json. DO NOT EDIT.\n");
    for (name, schema) in spec["components"]["schemas"].as_object().unwrap() {
        if schema["type"] != "object" {
            out.push_str(&format!("pub type {name} = {};\n", rust_type(schema)));
            continue;
        }
        let row = if schema["x-db-row"] == true {
            ", sqlx::FromRow"
        } else {
            ""
        };
        out.push_str(&format!("#[derive(Debug, Clone, serde::Serialize, serde::Deserialize{row})]\n#[serde(deny_unknown_fields)]\npub struct {name} {{\n"));
        for (field, prop) in schema["properties"].as_object().unwrap() {
            let required = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x == field);
            let ty = rust_type(prop);
            let ty = if !required || prop["nullable"] == true {
                format!("Option<{ty}>")
            } else {
                ty
            };
            let rust_field = if field == "totalElements" {
                "total_elements"
            } else {
                field
            };
            if prop["format"] == "decimal" {
                let suffix = if !required || prop["nullable"] == true {
                    "_option"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "#[serde(with = \"rust_decimal::serde::arbitrary_precision{suffix}\")]\n"
                ));
            }
            out.push_str(&format!(
                "#[serde(rename = {field:?})]\npub {rust_field}: {ty},\n"
            ));
        }
        out.push_str("}\n");
    }
    out.push_str("pub fn routes() -> axum::Router<crate::AppState> {\naxum::Router::new()\n");
    for (path, methods) in spec["paths"].as_object().unwrap() {
        for (method, op) in methods.as_object().unwrap() {
            let id = op["operationId"].as_str().unwrap();
            out.push_str(&format!(".route({path:?}, axum::routing::{method}(|axum::extract::State(state): axum::extract::State<crate::AppState>, req: axum::extract::Request| async move {{ crate::api::dispatch(state, req, {id:?}).await }}))\n"));
        }
    }
    out.push_str("}\n");
    let target = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("api.rs");
    fs::write(target, out).unwrap();
}
