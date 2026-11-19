// The HTTP benchmark server in Go: net/http and encoding/json.
package main

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
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

func main() {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /plaintext", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte("Hello, World!"))
	})
	mux.HandleFunc("GET /json", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(Message{"Hello, World!"})
	})
	mux.HandleFunc("GET /items", func(w http.ResponseWriter, r *http.Request) {
		items := make([]Item, 0, 20)
		for i := 0; i < 20; i++ {
			items = append(items, Item{i, fmt.Sprintf("item %d", i), float64(i) * 1.5, []string{"a", "b"}})
		}
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(items)
	})
	fmt.Fprintf(os.Stderr, "listening on %s\n", os.Args[1])
	http.ListenAndServe("127.0.0.1:"+os.Args[1], mux)
}
