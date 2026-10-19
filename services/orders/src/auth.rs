use crate::{
    AppState,
    error::{ApiError, Result},
    generated::*,
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use chrono::{Duration, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

// Internal JWT claims, not HTTP DTOs. HTTP types are generated from OpenAPI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub role: String,
    pub exp: i64,
    pub iat: i64,
    pub jti: Uuid,
    pub token_type: String,
    pub iss: String,
    pub aud: String,
}
pub fn decode_token(state: &AppState, token: &str, kind: &str) -> Result<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 0;
    validation.set_issuer(&["marketplace"]);
    validation.set_audience(&["marketplace-api"]);
    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|e| {
        let code = if kind == "refresh" {
            "REFRESH_TOKEN_INVALID"
        } else if matches!(e.kind(), jsonwebtoken::errors::ErrorKind::ExpiredSignature) {
            "TOKEN_EXPIRED"
        } else {
            "TOKEN_INVALID"
        };
        ApiError::new(code, "Токен недействителен или истёк")
    })?
    .claims;
    if claims.token_type != kind || !["USER", "SELLER", "ADMIN"].contains(&claims.role.as_str()) {
        return Err(ApiError::new(
            if kind == "refresh" {
                "REFRESH_TOKEN_INVALID"
            } else {
                "TOKEN_INVALID"
            },
            "Неверный тип токена",
        ));
    }
    Ok(claims)
}
pub fn authenticate(state: &AppState, headers: &axum::http::HeaderMap) -> Result<Claims> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::new("TOKEN_INVALID", "Требуется Bearer access token"))?;
    decode_token(state, token, "access")
}
async fn issue(
    state: &AppState,
    conn: &mut PgConnection,
    user: &UserResponse,
) -> Result<TokenPair> {
    let now = Utc::now();
    let mut claims = Claims {
        sub: user.id,
        role: user.role.clone(),
        exp: (now + Duration::minutes(20)).timestamp(),
        iat: now.timestamp(),
        jti: Uuid::new_v4(),
        token_type: "access".into(),
        iss: "marketplace".into(),
        aud: "marketplace-api".into(),
    };
    let key = EncodingKey::from_secret(state.jwt_secret.as_bytes());
    let access_token = encode(&Header::default(), &claims, &key)
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "Ошибка выпуска токена"))?;
    claims.token_type = "refresh".into();
    claims.jti = Uuid::new_v4();
    claims.exp = (now + Duration::days(14)).timestamp();
    let refresh_token = encode(&Header::default(), &claims, &key)
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "Ошибка выпуска токена"))?;
    sqlx::query("INSERT INTO refresh_tokens (id,user_id,expires_at) VALUES ($1,$2,$3)")
        .bind(claims.jti)
        .bind(user.id)
        .bind(now + Duration::days(14))
        .execute(conn)
        .await?;
    Ok(TokenPair {
        access_token,
        refresh_token,
        token_type: "Bearer".into(),
        expires_in: 1200,
    })
}
pub async fn register(state: &AppState, input: RegisterRequest) -> Result<UserResponse> {
    let hash = tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(input.password.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|h| h.to_string())
    })
    .await
    .map_err(|_| ApiError::new("INTERNAL_ERROR", "Ошибка хеширования"))?
    .map_err(|_| ApiError::new("INTERNAL_ERROR", "Ошибка хеширования"))?;
    let user = UserResponse {
        id: Uuid::new_v4(),
        email: input.email.to_lowercase(),
        role: input.role.unwrap_or_else(|| "USER".into()),
    };
    let result = sqlx::query("INSERT INTO users(id,email,password_hash,role) VALUES ($1,$2,$3,$4)")
        .bind(user.id)
        .bind(&user.email)
        .bind(hash)
        .bind(&user.role)
        .execute(&state.db)
        .await;
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ApiError::new(
            "EMAIL_ALREADY_EXISTS",
            "Email уже зарегистрирован",
        )),
        Err(e) => Err(e.into()),
        Ok(_) => Ok(user),
    }
}
pub async fn login(state: &AppState, input: LoginRequest) -> Result<TokenPair> {
    let row = sqlx::query("SELECT id,email,role,password_hash FROM users WHERE email=$1")
        .bind(input.email.to_lowercase())
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::new("TOKEN_INVALID", "Неверный email или пароль"))?;
    let hash: String = row.get("password_hash");
    let verified = tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|h| {
            Argon2::default()
                .verify_password(input.password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
    .map_err(|_| ApiError::new("INTERNAL_ERROR", "Ошибка проверки пароля"))?;
    if !verified {
        return Err(ApiError::new("TOKEN_INVALID", "Неверный email или пароль"));
    }
    let user = UserResponse {
        id: row.get("id"),
        email: row.get("email"),
        role: row.get("role"),
    };
    let mut tx = state.db.begin().await?;
    let tokens = issue(state, &mut tx, &user).await?;
    tx.commit().await?;
    Ok(tokens)
}
pub async fn refresh(state: &AppState, input: RefreshRequest) -> Result<TokenPair> {
    let claims = decode_token(state, &input.refresh_token, "refresh")?;
    let mut tx = state.db.begin().await?;
    // Atomic consume: simultaneous refreshes cannot reuse the same token.
    let used = sqlx::query("UPDATE refresh_tokens SET revoked=true WHERE id=$1 AND user_id=$2 AND NOT revoked AND expires_at > now() RETURNING id")
        .bind(claims.jti).bind(claims.sub).fetch_optional(&mut *tx).await?;
    if used.is_none() {
        return Err(ApiError::new(
            "REFRESH_TOKEN_INVALID",
            "Refresh token отозван или истёк",
        ));
    }
    let user = sqlx::query_as::<_, UserResponse>("SELECT id,email,role FROM users WHERE id=$1")
        .bind(claims.sub)
        .fetch_one(&mut *tx)
        .await?;
    let tokens = issue(state, &mut tx, &user).await?;
    tx.commit().await?;
    Ok(tokens)
}
