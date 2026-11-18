// Sort a million records with a comparator closure, then by two fields (milestone 4 benchmark).
package main

import (
	"fmt"
	"sort"
)

type Record struct {
	id    int
	score int
	name  string
}

func checksum(records []*Record) int {
	total := 0
	for i := 0; i < len(records); i += 1000 {
		total = (total*31 + records[i].id) % 1000000007
	}
	return total
}

func main() {
	n := 1000000
	seed := 42
	records := []*Record{}
	for i := 0; i < n; i++ {
		seed = (seed*1103515245 + 12345) % 2147483648
		records = append(records, &Record{i, seed % 100000, fmt.Sprintf("r%d", seed%1000)})
	}
	sort.SliceStable(records, func(a, b int) bool { return records[a].score < records[b].score })
	fmt.Println(checksum(records), records[0].score, records[n-1].score)
	byName := make([]*Record, n)
	copy(byName, records)
	sort.SliceStable(byName, func(a, b int) bool {
		x, y := byName[a], byName[b]
		if x.name != y.name {
			return x.name < y.name
		}
		return x.id < y.id
	})
	fmt.Println(checksum(byName), byName[0].name, byName[n-1].name)
}
