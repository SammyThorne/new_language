# Encode and decode 20,000 typed orders (100,000 line items) ten times (milestone 7 benchmark).
# Decoding builds dataclasses from the parsed dicts, as typed decoding does.
import json
from dataclasses import asdict, dataclass


@dataclass
class Item:
    sku: str
    qty: int
    price: float


@dataclass
class Order:
    id: int
    customer: str
    paid: bool
    items: list[Item]
    note: str | None


def decode(text):
    return [
        Order(o["id"], o["customer"], o["paid"], [Item(i["sku"], i["qty"], i["price"]) for i in o["items"]], o["note"])
        for o in json.loads(text)
    ]


def main():
    orders = []
    for i in range(20000):
        items = [Item(f"SKU-{i % 100}-{j}", j + 1, float(i % 1000) / 10.0 + float(j)) for j in range(5)]
        note = None if i % 3 == 0 else f"note {i}"
        orders.append(Order(i, f"customer {i}", i % 2 == 0, items, note))
    items_seen = 0
    quantity = 0
    value = 0.0
    for _ in range(10):
        text = json.dumps([asdict(o) for o in orders], separators=(",", ":"))
        back = decode(text)
        for order in back:
            items_seen += len(order.items)
            for item in order.items:
                quantity += item.qty
                value += item.price
    print(items_seen, quantity, int(value))


main()
