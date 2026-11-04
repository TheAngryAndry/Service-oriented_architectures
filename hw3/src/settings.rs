use std::time::Duration;

pub fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = std::env::var(name)?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be empty").into());
    }
    Ok(value)
}
pub fn number(name: &str, default: u64) -> Result<u64, Box<dyn std::error::Error>> {
    let value = std::env::var(name)
        .map(|v| v.parse())
        .unwrap_or(Ok(default))?;
    if value == 0 {
        return Err(format!("{name} must be positive").into());
    }
    Ok(value)
}
pub fn duration(name: &str, default_ms: u64) -> Result<Duration, Box<dyn std::error::Error>> {
    Ok(Duration::from_millis(number(name, default_ms)?))
}
pub fn logging() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
}
