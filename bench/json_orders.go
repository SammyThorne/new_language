// Encode and decode 20,000 typed orders (100,000 line items) ten times (milestone 7 benchmark),
// with encoding/json and struct tags.
package main

import (
	"encoding/json"
	"fmt"
)

type Item struct {
	Sku   string  `json:"sku"`
	Qty   int     `json:"qty"`
	Price float64 `json:"price"`
}

type Order struct {
	ID       int     `json:"id"`
	Customer string  `json:"customer"`
	Paid     bool    `json:"paid"`
	Items    []Item  `json:"items"`
	Note     *string `json:"note"`
}

func main() {
	orders := []Order{}
	for i := 0; i < 20000; i++ {
		items := []Item{}
		for j := 0; j < 5; j++ {
			items = append(items, Item{fmt.Sprintf("SKU-%d-%d", i%100, j), j + 1, float64(i%1000)/10.0 + float64(j)})
		}
		var note *string
		if i%3 != 0 {
			text := fmt.Sprintf("note %d", i)
			note = &text
		}
		orders = append(orders, Order{i, fmt.Sprintf("customer %d", i), i%2 == 0, items, note})
	}
	itemsSeen, quantity, value := 0, 0, 0.0
	for round := 0; round < 10; round++ {
		text, err := json.Marshal(orders)
		if err != nil {
			panic(err)
		}
		var back []Order
		if err := json.Unmarshal(text, &back); err != nil {
			panic(err)
		}
		for _, order := range back {
			itemsSeen += len(order.Items)
			for _, item := range order.Items {
				quantity += item.Qty
				value += item.Price
			}
		}
	}
	fmt.Println(itemsSeen, quantity, int(value))
}
