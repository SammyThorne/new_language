package main

import "fmt"

func main() {
	n := 20_000_000
	// Grown by appending, like the other versions, rather than allocated at full size.
	flags := []bool{}
	for i := 0; i <= n; i++ {
		flags = append(flags, true)
	}
	flags[0] = false
	flags[1] = false
	for i := 2; i*i <= n; i++ {
		if flags[i] {
			for j := i * i; j <= n; j += i {
				flags[j] = false
			}
		}
	}
	count := 0
	for _, flag := range flags {
		if flag {
			count++
		}
	}
	fmt.Println(count)
}
