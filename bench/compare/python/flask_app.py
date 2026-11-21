"""Flask server for bench/compare, run by Gunicorn with threaded workers (which keep connections
alive). It has no WebSocket endpoint: Flask needs an extension for that.

Usage: gunicorn -k gthread -w N --threads 16 -b 127.0.0.1:PORT flask_app:app
"""

from flask import Flask, jsonify, request

app = Flask(__name__)


@app.get("/json")
def json_message():
    return jsonify(message="Hello, World!")


@app.get("/items")
def items():
    return jsonify([{"id": i, "name": f"item {i}", "price": i * 1.5, "tags": ["a", "b"]} for i in range(20)])


@app.get("/users/<int:id>")
def user(id):
    return jsonify(id=id, name=f"user {id}", email=f"user{id}@example.com", active=id % 2 == 0)


@app.post("/orders")
def order():
    body = request.get_json()
    total = 0.0
    count = 0
    for line in body["lines"]:
        total += line["qty"] * line["price"]
        count += line["qty"]
    return jsonify(customer=body["customer"], count=count, total=total)
