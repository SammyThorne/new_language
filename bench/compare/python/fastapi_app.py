"""FastAPI server for bench/compare, run by Uvicorn (uvloop, httptools).

Usage: uvicorn fastapi_app:app --port PORT --workers N
"""

from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from pydantic import BaseModel

app = FastAPI()


class Message(BaseModel):
    message: str


class Item(BaseModel):
    id: int
    name: str
    price: float
    tags: list[str]


class User(BaseModel):
    id: int
    name: str
    email: str
    active: bool


class Line(BaseModel):
    sku: str
    qty: int
    price: float


class Order(BaseModel):
    customer: str
    lines: list[Line]


class Receipt(BaseModel):
    customer: str
    count: int
    total: float


@app.get("/json")
async def json_message() -> Message:
    return Message(message="Hello, World!")


@app.get("/items")
async def items() -> list[Item]:
    return [Item(id=i, name=f"item {i}", price=i * 1.5, tags=["a", "b"]) for i in range(20)]


@app.get("/users/{id}")
async def user(id: int) -> User:
    return User(id=id, name=f"user {id}", email=f"user{id}@example.com", active=id % 2 == 0)


@app.post("/orders")
async def order(body: Order) -> Receipt:
    total = 0.0
    count = 0
    for line in body.lines:
        total += line.qty * line.price
        count += line.qty
    return Receipt(customer=body.customer, count=count, total=total)


@app.websocket("/ws")
async def echo(socket: WebSocket):
    await socket.accept()
    try:
        while True:
            await socket.send_text(await socket.receive_text())
    except WebSocketDisconnect:
        pass
