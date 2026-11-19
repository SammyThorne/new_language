// Task creation: start 10,000 goroutines that each add two numbers, collect their results, twenty
// times. Each goroutine reports through its own buffered channel, as a joinable task does.
package main

import "fmt"

func add(a, b int, out chan<- int) { out <- a + b }

func main() {
	total := 0
	for round := 0; round < 20; round++ {
		results := make([]chan int, 10000)
		for i := range results {
			results[i] = make(chan int, 1)
			go add(i, round, results[i])
		}
		for _, result := range results {
			total += <-result
		}
	}
	fmt.Println(total)
}
