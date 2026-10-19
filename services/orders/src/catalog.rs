use crate::{
    AppState,
    auth::Claims,
    error::{ApiError, Result},
    generated::*,
};
use sqlx::PgConnection;
use uuid::Uuid;

pub async fn get(state: &AppState, id: Uuid) -> Result<ProductResponse> {
    sqlx::query_as("SELECT * FROM products WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::new("PRODUCT_NOT_FOUND", "Товар не найден"))
}
pub async fn create(state: &AppState, user: &Claims, p: ProductCreate) -> Result<ProductResponse> {
    Ok(sqlx::query_as("INSERT INTO products(id,name,description,price,stock,category,status,seller_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *")
        .bind(Uuid::new_v4()).bind(p.name).bind(p.description).bind(p.price).bind(p.stock).bind(p.category).bind(p.status).bind(user.sub).fetch_one(&state.db).await?)
}
pub async fn locked(conn: &mut PgConnection, id: Uuid) -> Result<ProductResponse> {
    sqlx::query_as("SELECT * FROM products WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| ApiError::new("PRODUCT_NOT_FOUND", "Товар не найден"))
}
fn ownership(user: &Claims, p: &ProductResponse) -> Result<()> {
    if user.role != "ADMIN" && p.seller_id != user.sub {
        Err(ApiError::new(
            "ACCESS_DENIED",
            "Товар принадлежит другому продавцу",
        ))
    } else {
        Ok(())
    }
}
pub async fn update(
    state: &AppState,
    user: &Claims,
    id: Uuid,
    p: ProductUpdate,
) -> Result<ProductResponse> {
    let mut tx = state.db.begin().await?;
    ownership(user, &locked(&mut tx, id).await?)?;
    let result = sqlx::query_as("UPDATE products SET name=$2,description=$3,price=$4,stock=$5,category=$6,status=$7 WHERE id=$1 RETURNING *")
        .bind(id).bind(p.name).bind(p.description).bind(p.price).bind(p.stock).bind(p.category).bind(p.status).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(result)
}
pub async fn archive(state: &AppState, user: &Claims, id: Uuid) -> Result<()> {
    let mut tx = state.db.begin().await?;
    ownership(user, &locked(&mut tx, id).await?)?;
    sqlx::query("UPDATE products SET status='ARCHIVED' WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
pub async fn list(
    state: &AppState,
    page: i32,
    size: i32,
    status: Option<String>,
    category: Option<String>,
) -> Result<ProductPage> {
    // One snapshot for count and page, even under concurrent inserts/deletes.
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let total_elements = sqlx::query_scalar("SELECT count(*) FROM products WHERE ($1::text IS NULL OR status=$1) AND ($2::text IS NULL OR category=$2)")
        .bind(&status).bind(&category).fetch_one(&mut *tx).await?;
    let items = sqlx::query_as("SELECT * FROM products WHERE ($1::text IS NULL OR status=$1) AND ($2::text IS NULL OR category=$2) ORDER BY created_at,id LIMIT $3 OFFSET $4")
        .bind(status).bind(category).bind(i64::from(size)).bind(i64::from(page)*i64::from(size)).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(ProductPage {
        items,
        total_elements,
        page,
        size,
    })
}
pub async fn create_promo(state: &AppState, p: PromoCreate) -> Result<PromoResponse> {
    if p.valid_until <= p.valid_from {
        return Err(ApiError::validation(
            "body.valid_until",
            "Должно быть позже valid_from",
        ));
    }
    let result = sqlx::query_as("INSERT INTO promo_codes(id,code,discount_type,discount_value,min_order_amount,max_uses,valid_from,valid_until,active) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *")
        .bind(Uuid::new_v4()).bind(p.code).bind(p.discount_type).bind(p.discount_value).bind(p.min_order_amount).bind(p.max_uses).bind(p.valid_from).bind(p.valid_until).bind(p.active).fetch_one(&state.db).await;
    match result {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ApiError::new(
            "PROMO_CODE_EXISTS",
            "Промокод уже существует",
        )),
        Err(e) => Err(e.into()),
        Ok(p) => Ok(p),
    }
}
