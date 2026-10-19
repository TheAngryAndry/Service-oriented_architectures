use chrono::{Duration, Utc};
use marketplace_tools::{Api, Result, check_logs, redact, require};
use serde_json::{Value, json};
use std::{
    env, io,
    path::Path,
    process::{Command, ExitCode},
};
use uuid::Uuid;

fn run(program: &str, args: &[&str]) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("Missing project root")?;
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .status()?;
    require(status.success(), &format!("{program} failed: {status}"))
}

fn cargo(args: &[&str]) -> Result<()> {
    run(&env::var("CARGO").unwrap_or_else(|_| "cargo".into()), args)
}

fn docker(args: &[&str], sudo: bool) -> Result<()> {
    if sudo {
        let mut full = vec!["-n", "docker"];
        full.extend_from_slice(args);
        run("sudo", &full)
    } else {
        run("docker", args)
    }
}

fn demo_call(api: &Api, method: &str, path: &str, body: Value, token: &str) -> Result<Value> {
    let response = api.request(method, path, &body, token)?;
    println!(
        "\n{method} {path} → {}; X-Request-Id: {}",
        response.status, response.request_id
    );
    let mut visible = response.body.clone();
    redact(&mut visible);
    println!("{}", serde_json::to_string_pretty(&visible)?);
    require(
        response.status < 400,
        "Демонстрация прервана: запрос завершился ошибкой",
    )?;
    Ok(response.body)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("Missing field {key}").into())
}

fn demo_account(api: &Api, run: &str, role: &str) -> Result<String> {
    let credentials = json!({"email":format!("demo-{run}-{}@example.test", role.to_lowercase()),"password":"demo-password-123"});
    let mut body = credentials.clone();
    body["role"] = json!(role);
    demo_call(api, "POST", "/auth/register", body, "")?;
    let tokens = demo_call(api, "POST", "/auth/login", credentials, "")?;
    Ok(text(&tokens, "access_token")?.to_owned())
}

fn demo() -> Result<()> {
    let api = Api::new()?;
    let run = Uuid::new_v4().simple().to_string()[..12].to_owned();
    let seller = demo_account(&api, &run, "SELLER")?;
    let buyer = demo_account(&api, &run, "USER")?;
    let product = demo_call(
        &api,
        "POST",
        "/products",
        json!({"name":"Учебник SOA","description":"Демо ДЗ 2","price":100,"stock":10,"category":"books","status":"ACTIVE"}),
        &seller,
    )?;
    let now = Utc::now();
    let promo = demo_call(
        &api,
        "POST",
        "/promo-codes",
        json!({"code":format!("DEMO_{}",run.to_uppercase()),"discount_type":"PERCENTAGE","discount_value":10,"min_order_amount":200,"max_uses":5,"active":true,"valid_from":(now-Duration::minutes(1)).to_rfc3339(),"valid_until":(now+Duration::days(1)).to_rfc3339()}),
        &seller,
    )?;
    let order = demo_call(
        &api,
        "POST",
        "/orders",
        json!({"items":[{"product_id":product["id"],"quantity":3}],"promo_code":promo["code"]}),
        &buyer,
    )?;
    let product_id = text(&product, "id")?;
    let order_id = text(&order, "id")?;
    let promo_id = text(&promo, "id")?;
    // The identifiers are also printed in SQL for the defense; validate before interpolation.
    for id in [product_id, order_id, promo_id] {
        Uuid::parse_str(id)?;
    }
    let product_path = format!("/products/{product_id}");
    let order_path = format!("/orders/{order_id}");
    demo_call(&api, "GET", &product_path, Value::Null, &buyer)?;
    demo_call(&api, "GET", &order_path, Value::Null, &buyer)?;
    demo_call(
        &api,
        "PUT",
        &order_path,
        json!({"items":[{"product_id":product_id,"quantity":1}]}),
        &buyer,
    )?;
    demo_call(
        &api,
        "POST",
        &format!("{order_path}/cancel"),
        Value::Null,
        &buyer,
    )?;
    demo_call(&api, "GET", &product_path, Value::Null, &buyer)?;
    println!("\nSELECT для проверки этого запуска:");
    println!("SELECT id,price,stock,status FROM products WHERE id='{product_id}';");
    println!(
        "SELECT id,status,total_amount,discount_amount,promo_code_id FROM orders WHERE id='{order_id}';"
    );
    println!("SELECT * FROM order_items WHERE order_id='{order_id}';");
    println!("SELECT code,current_uses FROM promo_codes WHERE id='{promo_id}';");
    Ok(())
}

fn execute() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["demo"] => demo(),
        ["smoke"] => {
            let response = Api::new()?.request("GET", "/health", &Value::Null, "")?;
            require(
                response.status == 200 && response.body["status"] == "ok",
                "Health check failed",
            )?;
            println!("PASS: health and OpenAPI");
            Ok(())
        }
        ["check-logs"] => {
            let count = check_logs(io::stdin().lock())?;
            println!("PASS: {count} JSON API log records, required fields and secret masking");
            Ok(())
        }
        ["generate-api"] => cargo(&[
            "build",
            "--locked",
            "--manifest-path",
            "services/orders/Cargo.toml",
        ]),
        ["test"] | ["test", "--sudo"] => {
            cargo(&[
                "test",
                "--locked",
                "--manifest-path",
                "services/orders/Cargo.toml",
            ])?;
            cargo(&["test", "--locked", "--manifest-path", "tools/Cargo.toml"])?;
            let sudo = args.len() == 2;
            docker(
                &[
                    "compose",
                    "up",
                    "--build",
                    "-d",
                    "--wait",
                    "--wait-timeout",
                    "180",
                ],
                sudo,
            )?;
            docker(
                &[
                    "compose",
                    "--profile",
                    "test",
                    "run",
                    "--build",
                    "--rm",
                    "e2e",
                ],
                sudo,
            )
        }
        [] | ["--help"] | ["help"] => {
            println!("cargo xtask <demo|smoke|check-logs|generate-api|test [--sudo]>");
            Ok(())
        }
        _ => Err("Usage: cargo xtask <demo|smoke|check-logs|generate-api|test [--sudo]>".into()),
    }
}

fn main() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
