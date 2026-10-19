"""Non-destructive demo: creates fresh seller/buyer/products for each run."""
import datetime as dt
import json
import os
import urllib.error
import urllib.request
import uuid

BASE = os.environ.get("API_URL", "http://localhost:8080")
RUN = uuid.uuid4().hex[:12]


def call(method, path, body=None, token=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    req = urllib.request.Request(BASE + path, json.dumps(body).encode() if body is not None else None, headers, method=method)
    try:
        response = urllib.request.urlopen(req, timeout=15)
    except urllib.error.HTTPError as e:
        response = e
    with response:
        result = json.load(response)
        print(f"\n{method} {path} → {response.status}; X-Request-Id: {response.headers.get('X-Request-Id')}")
        visible = {k: ("***" if k.endswith("token") else v) for k, v in result.items()}
        print(json.dumps(visible, ensure_ascii=False, indent=2))
        if response.status >= 400:
            raise SystemExit("Демонстрация прервана: запрос завершился ошибкой")
        return result


def account(role):
    credentials = dict(email=f"demo-{RUN}-{role.lower()}@example.test", password="demo-password-123")
    call("POST", "/auth/register", dict(credentials, role=role))
    return call("POST", "/auth/login", credentials)["access_token"]


seller, buyer = account("SELLER"), account("USER")
product = call("POST", "/products", dict(name="Учебник SOA", description="Демо ДЗ 2", price=100, stock=10, category="books", status="ACTIVE"), seller)
now = dt.datetime.now(dt.timezone.utc)
promo = call("POST", "/promo-codes", dict(code="DEMO_"+RUN.upper(), discount_type="PERCENTAGE", discount_value=10, min_order_amount=200, max_uses=5, active=True,
    valid_from=(now-dt.timedelta(minutes=1)).isoformat(), valid_until=(now+dt.timedelta(days=1)).isoformat()), seller)
order = call("POST", "/orders", dict(items=[dict(product_id=product["id"], quantity=3)], promo_code=promo["code"]), buyer)
call("GET", "/products/"+product["id"], token=buyer)
call("GET", "/orders/"+order["id"], token=buyer)
call("PUT", "/orders/"+order["id"], dict(items=[dict(product_id=product["id"], quantity=1)]), buyer)
call("POST", "/orders/"+order["id"]+"/cancel", token=buyer)
call("GET", "/products/"+product["id"], token=buyer)
print("\nSELECT для проверки этого запуска:")
print(f"SELECT id,price,stock,status FROM products WHERE id='{product['id']}';")
print(f"SELECT id,status,total_amount,discount_amount,promo_code_id FROM orders WHERE id='{order['id']}';")
print(f"SELECT * FROM order_items WHERE order_id='{order['id']}';")
print(f"SELECT code,current_uses FROM promo_codes WHERE id='{promo['id']}';")
