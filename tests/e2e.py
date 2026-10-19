"""Real HTTP + PostgreSQL tests. Creates isolated fixtures; never truncates tables.
Run against a disposable development database with ORDER_INTERVAL_MINUTES=1.
Python standard library only; psql must be on PATH.
"""
import base64
import concurrent.futures
import datetime as dt
import hashlib
import hmac
import json
import os
import re
import subprocess
import time
import unittest
import urllib.error
import urllib.request
import uuid
from decimal import Decimal

BASE = os.environ.get("API_URL", "http://localhost:8080")
PASSWORD = "test-password-123"


def sql(statement):
    return subprocess.check_output(["psql", "-X", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-c", statement], text=True).strip()


def request(method, path, body=None, token=None, raw=None):
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
    req = urllib.request.Request(BASE + path, data=data, method=method, headers=headers)
    try:
        response = urllib.request.urlopen(req, timeout=30)
    except urllib.error.HTTPError as e:
        response = e
    with response:
        payload = response.read()
        result = json.loads(payload, parse_float=Decimal) if payload else None
        uuid.UUID(response.headers["X-Request-Id"])
        if response.status >= 400:
            assert result["error_code"] in SPEC["x-error-statuses"], result
            assert SPEC["x-error-statuses"][result["error_code"]] == response.status, result
            assert isinstance(result["message"], str), result
        path_only = path.split("?")[0]
        for template, methods in SPEC["paths"].items():
            if re.fullmatch(re.escape(template).replace(r"\{id\}", "[^/]+"), path_only) and method.lower() in methods:
                contract_response = methods[method.lower()]["responses"][str(response.status)]
                if "$ref" in contract_response:
                    contract_response = SPEC["components"]["responses"][contract_response["$ref"].split("/")[-1]]
                content = contract_response.get("content", {})
                if content:
                    validate_response(content["application/json"]["schema"], result)
                else:
                    assert result is None, result
                break
        return response.status, result


def validate_response(schema, value):
    """Check response shapes/types against the published contract on every call."""
    if "$ref" in schema:
        schema = SPEC["components"]["schemas"][schema["$ref"].split("/")[-1]]
    if value is None and schema.get("nullable"):
        return
    kind = schema["type"]
    if kind == "object":
        assert isinstance(value, dict), (schema, value)
        assert all(k in value for k in schema.get("required", [])), value
        props = schema.get("properties", {})
        if schema.get("additionalProperties") is False:
            assert set(value) <= set(props), value
        for key, item in value.items():
            if key in props:
                validate_response(props[key], item)
    elif kind == "array":
        assert isinstance(value, list), value
        for item in value:
            validate_response(schema["items"], item)
    elif kind == "number":
        assert isinstance(value, (int, Decimal)) and not isinstance(value, bool), value
    elif kind == "integer":
        assert type(value) is int, value
    elif kind == "boolean":
        assert type(value) is bool, value
    elif kind == "string":
        assert isinstance(value, str), value
        if schema.get("format") == "uuid":
            uuid.UUID(value)
        elif schema.get("format") == "date-time":
            dt.datetime.fromisoformat(value)
    if "enum" in schema:
        assert value in schema["enum"], value


SPEC = json.load(urllib.request.urlopen(BASE + "/openapi.json", timeout=30))


class MarketplaceTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.seller, cls.seller_id = cls.account("SELLER")
        cls.admin, cls.admin_id = cls.account("ADMIN")

    @staticmethod
    def account(role="USER"):
        email = uuid.uuid4().hex + "@test.example"
        status, user = request("POST", "/auth/register", {"email": email, "password": PASSWORD, "role": "USER" if role == "ADMIN" else role})
        assert status == 201, user
        if role == "ADMIN":
            sql(f"UPDATE users SET role='ADMIN' WHERE id='{user['id']}'")
        status, tokens = request("POST", "/auth/login", {"email": email, "password": PASSWORD})
        assert status == 200, tokens
        return tokens["access_token"], user["id"]

    def call(self, method, path, body=None, token=None, status=200, code=None):
        actual, result = request(method, path, body, token)
        self.assertEqual(actual, status, result)
        if code:
            self.assertEqual(result["error_code"], code, result)
        return result

    def product(self, **overrides):
        data = dict(name="Book", description=None, price=100, stock=20, category=uuid.uuid4().hex, status="ACTIVE")
        data.update(overrides)
        return self.call("POST", "/products", data, self.seller, status=201)

    def promo(self, **overrides):
        now = dt.datetime.now(dt.timezone.utc)
        data = dict(code="P_" + uuid.uuid4().hex[:12].upper(), discount_type="PERCENTAGE", discount_value=10, min_order_amount=0, max_uses=10, active=True,
                    valid_from=(now-dt.timedelta(days=1)).isoformat(), valid_until=(now+dt.timedelta(days=1)).isoformat())
        data.update(overrides)
        return self.call("POST", "/promo-codes", data, self.seller, status=201)

    def order(self, token, p, quantity=1, **extra):
        return self.call("POST", "/orders", dict(items=[dict(product_id=p["id"], quantity=quantity)], **extra), token, status=201)

    def stock(self, p):
        return int(sql(f"SELECT stock FROM products WHERE id='{p['id']}'"))

    def age(self, user):
        sql(f"UPDATE user_operations SET created_at=now()-interval '2 days' WHERE user_id='{user}'")

    def test_products_crud_filter_and_validation(self):
        token, _ = self.account()
        p = self.product()
        self.assertEqual(p["seller_id"], self.seller_id)
        self.assertEqual(self.call("GET", "/products/"+p["id"], token=token)["id"], p["id"])
        listing = self.call("GET", "/products?category="+p["category"]+"&status=ACTIVE&size=1", token=token)
        self.assertEqual((listing["totalElements"], listing["page"], listing["size"], len(listing["items"])), (1,0,1,1))
        listing = self.call("GET", "/products?category="+p["category"]+"&page=1&size=1", token=token)
        self.assertEqual((listing["totalElements"], listing["items"]), (1,[]))
        for query in ["page=-1", "size=0", "size=101", "status=BAD", "page=no", "size=1&size=2"]:
            self.call("GET", "/products?"+query, token=token, status=400, code="VALIDATION_ERROR")
        invalid = dict(name="", price=0, stock=-1, category="", status="BAD")
        errors = self.call("POST", "/products", invalid, self.seller, status=400, code="VALIDATION_ERROR")
        self.assertEqual(len(errors["details"]["violations"]), 5)
        for overrides in [dict(name="x"*256), dict(description="x"*4001), dict(category="x"*101), dict(price=0.001), dict(stock=1.5), dict(seller_id=self.admin_id)]:
            body = {k:p[k] for k in ("name", "description", "price", "stock", "category", "status")}
            body["price"] = 100
            body.update(overrides)
            self.call("POST", "/products", body, self.seller, status=400, code="VALIDATION_ERROR")
        body = dict(name="Updated", price=125, stock=10, category=p["category"], status="INACTIVE")
        updated = self.call("PUT", "/products/"+p["id"], body, self.seller)
        self.assertEqual(updated["created_at"], p["created_at"])
        self.assertGreater(updated["updated_at"], p["updated_at"])
        self.call("DELETE", "/products/"+p["id"], token=self.seller, status=204)
        self.assertEqual(sql(f"SELECT status FROM products WHERE id='{p['id']}'"), "ARCHIVED")
        self.call("GET", "/products/"+str(uuid.uuid4()), token=token, status=404, code="PRODUCT_NOT_FOUND")
        self.call("GET", "/products/not-uuid", token=token, status=400, code="VALIDATION_ERROR")

    def test_roles_and_ownership(self):
        buyer, _ = self.account()
        other, _ = self.account()
        seller, _ = self.account("SELLER")
        p = self.product()
        self.call("POST", "/products", {}, buyer, status=403, code="ACCESS_DENIED")
        self.call("DELETE", "/products/"+p["id"], token=seller, status=403, code="ACCESS_DENIED")
        self.call("POST", "/orders", {}, seller, status=403, code="ACCESS_DENIED")
        self.call("POST", "/promo-codes", {}, buyer, status=403, code="ACCESS_DENIED")
        self.call("POST", "/auth/register", dict(email="root@test.example", password=PASSWORD, role="ADMIN"), status=400, code="VALIDATION_ERROR")
        order = self.order(buyer, p)
        path = "/orders/"+order["id"]
        for method, endpoint, body in [("GET",path,None),("PUT",path,{"items":[{"product_id":p["id"],"quantity":2}]}),("POST",path+"/cancel",None)]:
            self.call(method, endpoint, body, other, status=403, code="ORDER_OWNERSHIP_VIOLATION")
        self.call("GET", path, token=seller, status=403, code="ACCESS_DENIED")
        self.call("GET", path, token=self.admin)
        self.call("POST", path+"/cancel", token=self.admin)
        self.call("DELETE", "/products/"+p["id"], token=self.admin, status=204)

    def test_atomic_create_and_shortage_details(self):
        buyer, user = self.account()
        p, q = self.product(stock=2), self.product(stock=0)
        body = {"items":[{"product_id":p["id"],"quantity":3},{"product_id":q["id"],"quantity":1}]}
        err = self.call("POST", "/orders", body, buyer, status=409, code="INSUFFICIENT_STOCK")
        self.assertEqual(len(err["details"]["products"]), 2)
        body["items"] = [{"product_id":p["id"],"quantity":1}]
        body["promo_code"] = "MISSING"
        self.call("POST", "/orders", body, buyer, status=422, code="PROMO_CODE_INVALID")
        self.assertEqual(self.stock(p), 2)
        self.assertEqual(sql(f"SELECT count(*) FROM orders WHERE user_id='{user}'"), "0")
        self.assertEqual(sql(f"SELECT count(*) FROM user_operations WHERE user_id='{user}'"), "0")
        body.pop("promo_code")
        body["items"][0]["product_id"] = str(uuid.uuid4())
        self.call("POST", "/orders", body, buyer, status=404, code="PRODUCT_NOT_FOUND")
        inactive = self.product(status="INACTIVE")
        body["items"][0]["product_id"] = inactive["id"]
        self.call("POST", "/orders", body, buyer, status=409, code="PRODUCT_INACTIVE")

    def test_rate_order_priority_and_cancel(self):
        buyer, user = self.account()
        p = self.product(stock=5)
        promo = self.promo(max_uses=1)
        order = self.order(buyer, p, 2, promo_code=promo["code"])
        self.assertEqual((order["total_amount"],order["discount_amount"],self.stock(p)), (180,20,3))
        body = {"items":[{"product_id":p["id"],"quantity":1}]}
        self.call("POST", "/orders", body, buyer, status=429, code="ORDER_LIMIT_EXCEEDED")
        self.age(user)
        self.call("POST", "/orders", body, buyer, status=409, code="ORDER_HAS_ACTIVE")
        self.call("POST", "/orders/"+order["id"]+"/cancel", token=buyer)
        self.assertEqual(self.stock(p),5)
        self.assertEqual(sql(f"SELECT current_uses FROM promo_codes WHERE id='{promo['id']}'"), "0")
        self.call("POST", "/orders/"+order["id"]+"/cancel", token=buyer, status=409, code="INVALID_STATE_TRANSITION")
        self.assertEqual(self.stock(p),5)

    def test_update_snapshot_discount_removal_and_rollback(self):
        buyer, user = self.account()
        p = self.product(stock=10)
        promo = self.promo(min_order_amount=200, max_uses=1)
        order = self.order(buyer, p, 3, promo_code=promo["code"])
        path = "/orders/"+order["id"]
        # Changing live price cannot change an existing price snapshot.
        sql(f"UPDATE products SET price=999 WHERE id='{p['id']}'")
        current = self.call("GET", path, token=buyer)
        self.assertEqual(current["items"][0]["price_at_order"],100)
        updated = self.call("PUT", path, {"items":[{"product_id":p["id"],"quantity":2}]}, buyer)
        self.assertEqual((updated["total_amount"], updated["discount_amount"],self.stock(p)),(180,20,8))
        self.call("PUT", path, {"items":[{"product_id":p["id"],"quantity":1}]}, buyer, status=429, code="ORDER_LIMIT_EXCEEDED")
        self.age(user)
        updated = self.call("PUT", path, {"items":[{"product_id":p["id"],"quantity":1}]}, buyer)
        self.assertEqual((updated["total_amount"],updated["discount_amount"],updated["promo_code_id"]),(100,0,None))
        self.assertEqual(sql(f"SELECT current_uses FROM promo_codes WHERE id='{promo['id']}'"),"0")
        self.age(user)
        self.call("PUT", path, {"items":[{"product_id":p["id"],"quantity":11}]}, buyer, status=409, code="INSUFFICIENT_STOCK")
        self.assertEqual(self.stock(p),9)
        self.assertEqual(self.call("GET",path,token=buyer),updated)
        self.assertEqual(sql(f"SELECT count(*) FROM user_operations WHERE user_id='{user}' AND operation_type='UPDATE_ORDER'"),"2")

    def test_promo_validity_rounding_and_limits(self):
        p = self.product()
        buyer, _ = self.account()
        expired = self.promo(valid_from="2000-01-01T00:00:00Z",valid_until="2001-01-01T00:00:00Z")
        disabled = self.promo(active=False)
        future = self.promo(valid_from="2098-01-01T00:00:00Z",valid_until="2099-01-01T00:00:00Z")
        minimum = self.promo(min_order_amount=101)
        for promo, code in [(expired,"PROMO_CODE_INVALID"),(disabled,"PROMO_CODE_INVALID"),(future,"PROMO_CODE_INVALID"),(minimum,"PROMO_CODE_MIN_AMOUNT")]:
            self.call("POST","/orders",{"items":[{"product_id":p["id"],"quantity":1}],"promo_code":promo["code"]},buyer,status=422,code=code)
        for value, kind, expected in [(70,"PERCENTAGE",30),(71,"PERCENTAGE",100),(200,"FIXED_AMOUNT",0)]:
            buyer, _ = self.account()
            promo = self.promo(discount_value=value,discount_type=kind)
            self.assertEqual(self.order(buyer,p,promo_code=promo["code"])["total_amount"],expected)
        penny = self.product(price=0.05)
        buyer, _ = self.account()
        promo = self.promo(discount_value=10)
        self.assertEqual(self.order(buyer,penny,promo_code=promo["code"])["total_amount"],Decimal("0.04"))

    def test_invalid_promo_on_update_rolls_back(self):
        buyer, user = self.account()
        p = self.product()
        promo = self.promo()
        order = self.order(buyer,p,promo_code=promo["code"])
        sql(f"UPDATE promo_codes SET active=false WHERE id='{promo['id']}'")
        self.call("PUT","/orders/"+order["id"],{"items":[{"product_id":p["id"],"quantity":2}]},buyer,status=422,code="PROMO_CODE_INVALID")
        self.assertEqual(self.stock(p),19)
        self.assertEqual(self.call("GET","/orders/"+order["id"],token=buyer),order)
        self.assertEqual(sql(f"SELECT count(*) FROM user_operations WHERE user_id='{user}' AND operation_type='UPDATE_ORDER'"),"0")

    def test_state_machine(self):
        buyer, _ = self.account()
        p = self.product()
        order = self.order(buyer,p)
        path = "/orders/"+order["id"]
        self.call("POST",path+"/status",{"status":"PAID"},self.admin,status=409,code="INVALID_STATE_TRANSITION")
        self.call("POST",path+"/status",{"status":"PAYMENT_PENDING"},buyer,status=403,code="ACCESS_DENIED")
        self.call("POST",path+"/status",{"status":"PAYMENT_PENDING"},self.admin)
        self.call("PUT",path,{"items":[{"product_id":p["id"],"quantity":2}]},buyer,status=409,code="INVALID_STATE_TRANSITION")
        for state in ["PAID","SHIPPED","COMPLETED"]:
            self.assertEqual(self.call("POST",path+"/status",{"status":state},self.admin)["status"],state)
            self.call("POST",path+"/cancel",token=buyer,status=409,code="INVALID_STATE_TRANSITION")
        other, _ = self.account()
        order = self.order(other,p)
        self.call("POST","/orders/"+order["id"]+"/status",{"status":"PAYMENT_PENDING"},self.admin)
        self.call("POST","/orders/"+order["id"]+"/cancel",token=other)
        self.call("GET","/orders/"+str(uuid.uuid4()),token=buyer,status=404,code="ORDER_NOT_FOUND")

    def test_concurrent_stock_reservation(self):
        p = self.product(stock=1)
        buyers = [self.account()[0] for _ in range(4)]
        body = {"items":[{"product_id":p["id"],"quantity":1}]}
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda token: request("POST","/orders",body,token),buyers))
        self.assertEqual(sorted(status for status,_ in results),[201,409,409,409])
        self.assertEqual(self.stock(p),0)
        self.assertEqual(sql(f"SELECT sum(quantity) FROM order_items WHERE product_id='{p['id']}'"),"1")

    def test_concurrent_active_order_promo_refresh_and_cancel(self):
        buyer, user = self.account()
        p = self.product(stock=10)
        body = {"items":[{"product_id":p["id"],"quantity":1}]}
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: request("POST","/orders",body,buyer),range(2)))
        self.assertEqual(sorted(s for s,_ in results),[201,429])
        order = next(r for s,r in results if s==201)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: request("POST","/orders/"+order["id"]+"/cancel",token=buyer),range(2)))
        self.assertEqual(sorted(s for s,_ in results),[200,409])
        self.assertEqual(self.stock(p),10)
        promo = self.promo(max_uses=1)
        buyers = [self.account()[0] for _ in range(2)]
        body["promo_code"] = promo["code"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda t: request("POST","/orders",body,t),buyers))
        self.assertEqual(sorted(s for s,_ in results),[201,422])
        self.assertEqual(self.stock(p),9)
        self.assertEqual(sql(f"SELECT current_uses FROM promo_codes WHERE id='{promo['id']}'"),"1")

    def test_auth_refresh_and_expired_tokens(self):
        email = uuid.uuid4().hex+"@test.example"
        user = self.call("POST","/auth/register",dict(email=email,password=PASSWORD),status=201)
        self.assertEqual(user["role"],"USER")
        self.call("POST","/auth/register",dict(email=email.upper(),password=PASSWORD),status=409,code="EMAIL_ALREADY_EXISTS")
        self.call("POST","/auth/login",dict(email=email,password="wrong-password"),status=401,code="TOKEN_INVALID")
        tokens = self.call("POST","/auth/login",dict(email=email,password=PASSWORD))
        self.assertEqual(tokens["expires_in"],1200)
        self.call("GET","/products",status=401,code="TOKEN_INVALID")
        self.call("GET","/products",token=tokens["refresh_token"],status=401,code="TOKEN_INVALID")
        self.call("POST","/auth/refresh",{"refresh_token":tokens["access_token"]},status=401,code="REFRESH_TOKEN_INVALID")
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda _: request("POST","/auth/refresh",{"refresh_token":tokens["refresh_token"]}),range(2)))
        self.assertEqual(sorted(s for s,_ in results),[200,401])
        fresh = next(r for s,r in results if s==200)
        self.call("GET","/products",token=fresh["access_token"])
        self.call("POST","/auth/refresh",{"refresh_token":tokens["refresh_token"]},status=401,code="REFRESH_TOKEN_INVALID")
        secret = os.environ["JWT_SECRET"]
        parts = tokens["access_token"].split(".")
        claims = json.loads(base64.urlsafe_b64decode(parts[1]+"=="))
        claims["exp"] = int(time.time())-10
        def b64(data):
            return base64.urlsafe_b64encode(data).decode().rstrip("=")
        parts[1] = b64(json.dumps(claims).encode())
        payload = (parts[0]+"."+parts[1]).encode()
        expired = payload.decode()+"."+b64(hmac.new(secret.encode(),payload,hashlib.sha256).digest())
        self.call("GET","/products",token=expired,status=401,code="TOKEN_EXPIRED")
        self.call("GET","/products",token="invalid",status=401,code="TOKEN_INVALID")

    def test_order_input_and_error_envelopes(self):
        buyer, _ = self.account()
        p = self.product()
        item = {"product_id":p["id"],"quantity":1}
        for body in [{"items":[]},{"items":[item]*51},{"items":[item,item]}, {"items":[dict(item,quantity=0)]}, {"items":[dict(item,quantity=1000)]},{"items":[dict(item,product_id="bad")]},{"items":[item],"promo_code":"lowercase"}]:
            self.call("POST","/orders",body,buyer,status=400,code="VALIDATION_ERROR")
        self.assertEqual(request("POST","/orders",token=buyer,raw=b'{"password":"SECRET",')[0],400)
        self.call("GET","/unknown",status=404,code="NOT_FOUND")
        self.call("PATCH","/products",token=buyer,status=405,code="METHOD_NOT_ALLOWED")
        self.assertEqual(self.stock(p),20)


if __name__ == "__main__":
    unittest.main(verbosity=2)
