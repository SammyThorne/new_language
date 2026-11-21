// The Go server for bench/compare: net/http, encoding/json and gorilla/websocket, with the same
// endpoints as viper/server.vi. Usage: server PORT
package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"strconv"

	"github.com/gorilla/websocket"
)

type Message struct {
	Message string `json:"message"`
}

type Item struct {
	ID    int      `json:"id"`
	Name  string   `json:"name"`
	Price float64  `json:"price"`
	Tags  []string `json:"tags"`
}

type User struct {
	ID     int    `json:"id"`
	Name   string `json:"name"`
	Email  string `json:"email"`
	Active bool   `json:"active"`
}

type Line struct {
	SKU   string  `json:"sku"`
	Qty   int     `json:"qty"`
	Price float64 `json:"price"`
}

type Order struct {
	Customer string `json:"customer"`
	Lines    []Line `json:"lines"`
}

type Receipt struct {
	Customer string  `json:"customer"`
	Count    int     `json:"count"`
	Total    float64 `json:"total"`
}

func sendJSON(w http.ResponseWriter, value any) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(value)
}

var upgrader = websocket.Upgrader{ReadBufferSize: 1024, WriteBufferSize: 1024}

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /json", func(w http.ResponseWriter, r *http.Request) {
		sendJSON(w, Message{"Hello, World!"})
	})
	mux.HandleFunc("GET /items", func(w http.ResponseWriter, r *http.Request) {
		items := make([]Item, 0, 20)
		for i := 0; i < 20; i++ {
			items = append(items, Item{i, fmt.Sprintf("item %d", i), float64(i) * 1.5, []string{"a", "b"}})
		}
		sendJSON(w, items)
	})
	mux.HandleFunc("GET /users/{id}", func(w http.ResponseWriter, r *http.Request) {
		id, err := strconv.Atoi(r.PathValue("id"))
		if err != nil {
			http.Error(w, "id must be an integer", http.StatusBadRequest)
			return
		}
		sendJSON(w, User{id, fmt.Sprintf("user %d", id), fmt.Sprintf("user%d@example.com", id), id%2 == 0})
	})
	mux.HandleFunc("POST /orders", func(w http.ResponseWriter, r *http.Request) {
		var order Order
		if err := json.NewDecoder(r.Body).Decode(&order); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		receipt := Receipt{Customer: order.Customer}
		for _, line := range order.Lines {
			receipt.Total += float64(line.Qty) * line.Price
			receipt.Count += line.Qty
		}
		sendJSON(w, receipt)
	})
	mux.HandleFunc("GET /ws", func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()
		for {
			kind, data, err := conn.ReadMessage()
			if err != nil {
				return
			}
			if conn.WriteMessage(kind, data) != nil {
				return
			}
		}
	})
	fmt.Fprintf(os.Stderr, "listening on %s\n", os.Args[1])
	http.ListenAndServe("127.0.0.1:"+os.Args[1], mux)
}
