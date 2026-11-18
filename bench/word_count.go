package main

import (
	"fmt"
	"strings"
)

const text = "The language reads like Python and runs like a compiled program. Readability is the first goal, and speed is the second goal. The rest of the plan follows from those two goals: the shape of the syntax, the choice of a collector, the design of traits."

func main() {
	counts := map[string]int{}
	for i := 0; i < 500_000; i++ {
		for _, word := range strings.Fields(text) {
			key := strings.ToLower(word)
			// Written as a lookup and a store, like the other versions; Go compiles this
			// pattern to a single map access.
			counts[key] = counts[key] + 1
		}
	}
	fmt.Println(len(counts), counts["the"], counts["goal,"])
}
