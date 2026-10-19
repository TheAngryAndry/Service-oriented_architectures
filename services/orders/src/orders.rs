use crate::{
    AppState,
    auth::Claims,
    catalog,
    error::{ApiError, Result},
    generated::*,
};
use chrono::{DateTime, Duration, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::json;
use sqlx::{PgConnection, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;

async fn lock_user(conn: &mut PgConnection, user: Uuid) -> Result<()> {
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(user)
        .fetch_one(conn)
        .await?;
    Ok(())
}
async fn rate_limit(
    conn: &mut PgConnection,
    user: Uuid,
    operation: &str,
    minutes: i64,
) -> Result<()> {
    let last: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT created_at FROM user_operations WHERE user_id=$1 AND operation_type=$2 ORDER BY created_at DESC LIMIT 1")
        .bind(user).bind(operation).fetch_optional(conn).await?;
    if last.is_some_and(|last| Utc::now() < last + Duration::minutes(minutes)) {
        return Err(
            ApiError::new("ORDER_LIMIT_EXCEEDED", "Превышен лимит частоты операций")
                .details(json!({"interval_minutes":minutes,"operation":operation})),
        );
    }
    Ok(())
}
async fn record(conn: &mut PgConnection, user: Uuid, operation: &str) -> Result<()> {
    sqlx::query("INSERT INTO user_operations(id,user_id,operation_type,created_at) VALUES ($1,$2,$3,clock_timestamp())")
        .bind(Uuid::new_v4()).bind(user).bind(operation).execute(conn).await?;
    Ok(())
}
fn ownership(user: &Claims, order: &OrderResponse) -> Result<()> {
    if user.role != "ADMIN" && order.user_id != user.sub {
        Err(ApiError::new(
            "ORDER_OWNERSHIP_VIOLATION",
            "Заказ принадлежит другому пользователю",
        ))
    } else {
        Ok(())
    }
}
async fn load(conn: &mut PgConnection, id: Uuid, lock: bool) -> Result<OrderResponse> {
    let sql = if lock {
        "SELECT * FROM orders WHERE id=$1 FOR UPDATE"
    } else {
        "SELECT * FROM orders WHERE id=$1"
    };
    let row = sqlx::query(sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| ApiError::new("ORDER_NOT_FOUND", "Заказ не найден"))?;
    let items = sqlx::query("SELECT * FROM order_items WHERE order_id=$1 ORDER BY product_id")
        .bind(id)
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(|r| OrderItemResponse {
            id: r.get("id"),
            product_id: r.get("product_id"),
            quantity: r.get("quantity"),
            price_at_order: r.get("price_at_order"),
        })
        .collect();
    Ok(OrderResponse {
        id,
        user_id: row.get("user_id"),
        status: row.get("status"),
        promo_code_id: row.get("promo_code_id"),
        total_amount: row.get("total_amount"),
        discount_amount: row.get("discount_amount"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        items,
    })
}
pub async fn get(state: &AppState, user: &Claims, id: Uuid) -> Result<OrderResponse> {
    let mut tx = state.db.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let order = load(&mut tx, id, false).await?;
    ownership(user, &order)?;
    tx.commit().await?;
    Ok(order)
}
async fn begin_order(
    state: &AppState,
    user: &Claims,
    id: Uuid,
) -> Result<(Transaction<'static, Postgres>, OrderResponse)> {
    let mut tx = state.db.begin().await?;
    let owner: Uuid = sqlx::query_scalar("SELECT user_id FROM orders WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::new("ORDER_NOT_FOUND", "Заказ не найден"))?;
    // All order mutations follow user -> order -> sorted products -> promo.
    lock_user(&mut tx, owner).await?;
    let order = load(&mut tx, id, true).await?;
    ownership(user, &order)?;
    Ok((tx, order))
}
async fn lock_products(
    conn: &mut PgConnection,
    ids: impl Iterator<Item = Uuid>,
) -> Result<BTreeMap<Uuid, ProductResponse>> {
    let ids: std::collections::BTreeSet<_> = ids.collect();
    let mut products = BTreeMap::new();
    for id in ids {
        products.insert(id, catalog::locked(conn, id).await?);
    }
    Ok(products)
}
fn check_catalog(
    products: &BTreeMap<Uuid, ProductResponse>,
    items: &[OrderItemInput],
) -> Result<()> {
    for item in items {
        if products[&item.product_id].status != "ACTIVE" {
            return Err(ApiError::new("PRODUCT_INACTIVE", "Товар неактивен")
                .details(json!({"product_id":item.product_id})));
        }
    }
    let shortages: Vec<_> = items.iter().filter_map(|item| {
        let stock = products[&item.product_id].stock;
        (stock < item.quantity).then(|| json!({"product_id":item.product_id,"requested_quantity":item.quantity,"available_stock":stock}))
    }).collect();
    if !shortages.is_empty() {
        return Err(ApiError::new("INSUFFICIENT_STOCK", "Недостаточно остатков")
            .details(json!({"products":shortages})));
    }
    Ok(())
}
async fn reserve(conn: &mut PgConnection, items: &[OrderItemInput]) -> Result<()> {
    for item in items {
        sqlx::query("UPDATE products SET stock=stock-$2 WHERE id=$1")
            .bind(item.product_id)
            .bind(item.quantity)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}
async fn restore(
    conn: &mut PgConnection,
    products: &mut BTreeMap<Uuid, ProductResponse>,
    items: &[OrderItemResponse],
) -> Result<()> {
    for item in items {
        let p = products.get_mut(&item.product_id).unwrap();
        p.stock = p.stock.checked_add(item.quantity).ok_or_else(|| {
            ApiError::validation("stock", "Остаток после возврата превышает INTEGER")
        })?;
        sqlx::query("UPDATE products SET stock=$2 WHERE id=$1")
            .bind(item.product_id)
            .bind(p.stock)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}
fn snapshot(
    items: &[OrderItemInput],
    products: &BTreeMap<Uuid, ProductResponse>,
    old: &[OrderItemResponse],
) -> Vec<OrderItemResponse> {
    items
        .iter()
        .map(|item| OrderItemResponse {
            id: Uuid::new_v4(),
            product_id: item.product_id,
            quantity: item.quantity,
            price_at_order: old
                .iter()
                .find(|o| o.product_id == item.product_id)
                .map(|o| o.price_at_order)
                .unwrap_or(products[&item.product_id].price),
        })
        .collect()
}
fn subtotal(items: &[OrderItemResponse]) -> Result<Decimal> {
    let total: Decimal = items
        .iter()
        .map(|i| i.price_at_order * Decimal::from(i.quantity))
        .sum();
    if total > Decimal::new(999_999_999_999, 2) {
        return Err(ApiError::validation(
            "items",
            "Сумма превышает NUMERIC(12,2)",
        ));
    }
    Ok(total)
}
pub fn discount(total: Decimal, promo: &PromoResponse) -> Decimal {
    let amount = if promo.discount_type == "PERCENTAGE" {
        // Assignment: percentage >70 means full price (no discount), not a cap.
        if promo.discount_value > Decimal::from(70) {
            Decimal::ZERO
        } else {
            total * promo.discount_value / Decimal::from(100)
        }
    } else {
        total.min(promo.discount_value)
    };
    amount.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}
fn promo_valid(p: &PromoResponse, already_used: bool) -> Result<()> {
    let now = Utc::now();
    let usage = p.current_uses - i32::from(already_used);
    if !p.active || usage >= p.max_uses || now < p.valid_from || now > p.valid_until {
        return Err(ApiError::new(
            "PROMO_CODE_INVALID",
            "Промокод неактивен, истёк или исчерпан",
        ));
    }
    Ok(())
}
async fn change_usage(conn: &mut PgConnection, id: Uuid, delta: i32) -> Result<()> {
    sqlx::query("UPDATE promo_codes SET current_uses=current_uses+$2 WHERE id=$1")
        .bind(id)
        .bind(delta)
        .execute(conn)
        .await?;
    Ok(())
}
async fn insert_items(
    conn: &mut PgConnection,
    order: Uuid,
    items: &[OrderItemResponse],
) -> Result<()> {
    for item in items {
        sqlx::query("INSERT INTO order_items(id,order_id,product_id,quantity,price_at_order) VALUES ($1,$2,$3,$4,$5)")
            .bind(item.id).bind(order).bind(item.product_id).bind(item.quantity).bind(item.price_at_order).execute(&mut *conn).await?;
    }
    Ok(())
}
pub async fn create(state: &AppState, user: &Claims, input: OrderCreate) -> Result<OrderResponse> {
    let mut tx = state.db.begin().await?;
    lock_user(&mut tx, user.sub).await?;
    rate_limit(
        &mut tx,
        user.sub,
        "CREATE_ORDER",
        state.order_interval_minutes,
    )
    .await?;
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM orders WHERE user_id=$1 AND status IN ('CREATED','PAYMENT_PENDING'))")
        .bind(user.sub).fetch_one(&mut *tx).await?;
    if active {
        return Err(ApiError::new(
            "ORDER_HAS_ACTIVE",
            "У пользователя уже есть активный заказ",
        ));
    }
    let products = lock_products(&mut tx, input.items.iter().map(|i| i.product_id)).await?;
    check_catalog(&products, &input.items)?;
    reserve(&mut tx, &input.items).await?;
    let items = snapshot(&input.items, &products, &[]);
    let total = subtotal(&items)?;
    let mut applied = None;
    let mut saving = Decimal::ZERO;
    if let Some(code) = input.promo_code {
        let p: PromoResponse = sqlx::query_as("SELECT * FROM promo_codes WHERE code=$1 FOR UPDATE")
            .bind(code)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::new("PROMO_CODE_INVALID", "Промокод не найден"))?;
        promo_valid(&p, false)?;
        if total < p.min_order_amount {
            return Err(ApiError::new(
                "PROMO_CODE_MIN_AMOUNT",
                "Сумма ниже минимальной для промокода",
            )
            .details(json!({"min_order_amount":p.min_order_amount,"amount":total})));
        }
        saving = discount(total, &p);
        change_usage(&mut tx, p.id, 1).await?;
        applied = Some(p.id);
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO orders(id,user_id,status,promo_code_id,total_amount,discount_amount) VALUES ($1,$2,'CREATED',$3,$4,$5)")
        .bind(id).bind(user.sub).bind(applied).bind(total-saving).bind(saving).execute(&mut *tx).await?;
    insert_items(&mut tx, id, &items).await?;
    record(&mut tx, user.sub, "CREATE_ORDER").await?;
    let response = load(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(response)
}
pub async fn update(
    state: &AppState,
    user: &Claims,
    id: Uuid,
    input: OrderUpdate,
) -> Result<OrderResponse> {
    let (mut tx, old) = begin_order(state, user, id).await?;
    if old.status != "CREATED" {
        return Err(ApiError::new(
            "INVALID_STATE_TRANSITION",
            "Изменять можно только CREATED",
        ));
    }
    rate_limit(
        &mut tx,
        old.user_id,
        "UPDATE_ORDER",
        state.order_interval_minutes,
    )
    .await?;
    let mut products = lock_products(
        &mut tx,
        old.items
            .iter()
            .map(|i| i.product_id)
            .chain(input.items.iter().map(|i| i.product_id)),
    )
    .await?;
    restore(&mut tx, &mut products, &old.items).await?;
    check_catalog(&products, &input.items)?;
    reserve(&mut tx, &input.items).await?;
    let items = snapshot(&input.items, &products, &old.items);
    let total = subtotal(&items)?;
    let mut applied = old.promo_code_id;
    let mut saving = Decimal::ZERO;
    if let Some(promo_id) = applied {
        let p: PromoResponse = sqlx::query_as("SELECT * FROM promo_codes WHERE id=$1 FOR UPDATE")
            .bind(promo_id)
            .fetch_one(&mut *tx)
            .await?;
        promo_valid(&p, true)?;
        if total < p.min_order_amount {
            applied = None;
            change_usage(&mut tx, promo_id, -1).await?;
        } else {
            saving = discount(total, &p);
        }
    }
    sqlx::query("DELETE FROM order_items WHERE order_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    insert_items(&mut tx, id, &items).await?;
    sqlx::query(
        "UPDATE orders SET total_amount=$2,discount_amount=$3,promo_code_id=$4 WHERE id=$1",
    )
    .bind(id)
    .bind(total - saving)
    .bind(saving)
    .bind(applied)
    .execute(&mut *tx)
    .await?;
    record(&mut tx, old.user_id, "UPDATE_ORDER").await?;
    let response = load(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(response)
}
pub async fn cancel(state: &AppState, user: &Claims, id: Uuid) -> Result<OrderResponse> {
    let (mut tx, order) = begin_order(state, user, id).await?;
    if !["CREATED", "PAYMENT_PENDING"].contains(&order.status.as_str()) {
        return Err(ApiError::new(
            "INVALID_STATE_TRANSITION",
            "Отмена разрешена только из CREATED/PAYMENT_PENDING",
        ));
    }
    let mut products = lock_products(&mut tx, order.items.iter().map(|i| i.product_id)).await?;
    restore(&mut tx, &mut products, &order.items).await?;
    if let Some(id) = order.promo_code_id {
        // UPDATE acquires the promo row lock; order lock prevents double release.
        change_usage(&mut tx, id, -1).await?;
    }
    sqlx::query("UPDATE orders SET status='CANCELED' WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let response = load(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(response)
}
pub fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("CREATED", "PAYMENT_PENDING")
            | ("PAYMENT_PENDING", "PAID")
            | ("PAID", "SHIPPED")
            | ("SHIPPED", "COMPLETED")
    )
}
pub async fn transition(
    state: &AppState,
    id: Uuid,
    input: OrderTransition,
) -> Result<OrderResponse> {
    let mut tx = state.db.begin().await?;
    let order = load(&mut tx, id, true).await?;
    if !can_transition(&order.status, &input.status) {
        return Err(ApiError::new(
            "INVALID_STATE_TRANSITION",
            "Недопустимый переход состояния",
        ));
    }
    sqlx::query("UPDATE orders SET status=$2 WHERE id=$1")
        .bind(id)
        .bind(input.status)
        .execute(&mut *tx)
        .await?;
    let response = load(&mut tx, id, false).await?;
    tx.commit().await?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn promo(value: i64, kind: &str) -> PromoResponse {
        PromoResponse {
            id: Uuid::new_v4(),
            code: "TEST".into(),
            discount_type: kind.into(),
            discount_value: Decimal::from(value),
            min_order_amount: Decimal::ZERO,
            max_uses: 1,
            current_uses: 1,
            valid_from: Utc::now() - Duration::days(1),
            valid_until: Utc::now() + Duration::days(1),
            active: true,
        }
    }
    #[test]
    fn discount_rules_and_own_usage() {
        assert_eq!(
            discount(Decimal::from(100), &promo(70, "PERCENTAGE")),
            Decimal::from(70)
        );
        assert_eq!(
            discount(Decimal::from(100), &promo(71, "PERCENTAGE")),
            Decimal::ZERO
        );
        assert_eq!(
            discount(Decimal::from(100), &promo(200, "FIXED_AMOUNT")),
            Decimal::from(100)
        );
        assert_eq!(
            discount(Decimal::new(5, 2), &promo(10, "PERCENTAGE")),
            Decimal::new(1, 2)
        );
        assert!(promo_valid(&promo(10, "PERCENTAGE"), true).is_ok());
        assert!(promo_valid(&promo(10, "PERCENTAGE"), false).is_err());
    }
    #[test]
    fn only_forward_adjacent_states_allowed() {
        let states = [
            "CREATED",
            "PAYMENT_PENDING",
            "PAID",
            "SHIPPED",
            "COMPLETED",
            "CANCELED",
        ];
        for (i, from) in states.iter().enumerate() {
            for (j, to) in states.iter().enumerate() {
                assert_eq!(
                    can_transition(from, to),
                    i < 4 && j == i + 1,
                    "{from} -> {to}"
                );
            }
        }
    }
}
