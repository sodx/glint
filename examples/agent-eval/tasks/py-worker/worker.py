"""Nightly worker: prices every order in orders.json and writes totals.json."""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def unit_price(item):
    price = item["price"]
    # Bulk discount: 10 % off from the tenth unit on.
    discount = 0.1 if item["qty"] >= 10 else 0.0
    return round(price * (1 - discount), 2)


def order_total(order):
    total = 0.0
    for item in order["items"]:
        total += unit_price(item) * item["qty"]
    if order.get("coupon"):
        total -= total * order["coupon"]["percent"] / 100
    return round(total, 2)


def main():
    with open(os.path.join(HERE, "orders.json")) as f:
        orders = json.load(f)
    totals, skipped = {}, 0
    for order in orders:
        try:
            totals[order["id"]] = order_total(order)
        except Exception:
            skipped += 1
    with open(os.path.join(HERE, "totals.json"), "w") as f:
        json.dump(totals, f, indent=2, sort_keys=True)
    print(f"processed {len(totals)} orders, skipped {skipped}")


if __name__ == "__main__":
    main()
