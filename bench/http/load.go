// A small HTTP load generator: `connections` keep-alive connections each send GET requests one
// after another for `seconds`, and the program prints requests per second and latency
// percentiles. Written because no load-testing tool is installed on the benchmark machine.
//
// Usage: load PORT PATH CONNECTIONS SECONDS [METHOD BODY]
//
// With METHOD and BODY, each request is that method with BODY as a JSON body.
package main

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

func main() {
	port, path := os.Args[1], os.Args[2]
	connections, _ := strconv.Atoi(os.Args[3])
	seconds, _ := strconv.ParseFloat(os.Args[4], 64)
	request := []byte("GET " + path + " HTTP/1.1\r\nHost: localhost\r\n\r\n")
	if len(os.Args) > 6 {
		body := os.Args[6]
		request = []byte(os.Args[5] + " " + path + " HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: " +
			strconv.Itoa(len(body)) + "\r\n\r\n" + body)
	}
	deadline := time.Now().Add(time.Duration(seconds * float64(time.Second)))
	var mu sync.Mutex
	var latencies []time.Duration
	errors := 0
	var wg sync.WaitGroup
	for c := 0; c < connections; c++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			conn, err := net.Dial("tcp", "127.0.0.1:"+port)
			if err != nil {
				mu.Lock()
				errors++
				mu.Unlock()
				return
			}
			defer conn.Close()
			// A server that stops answering a connection must not hang the run: reads and writes
			// fail two seconds after the deadline, and the request is counted as an error.
			conn.SetDeadline(deadline.Add(2 * time.Second))
			reader := bufio.NewReaderSize(conn, 64*1024)
			local := make([]time.Duration, 0, 100000)
		Requests:
			for time.Now().Before(deadline) {
				start := time.Now()
				if _, err := conn.Write(request); err != nil {
					break
				}
				length := -1
				status, err := reader.ReadString('\n')
				if err != nil || !strings.Contains(status, " 200 ") {
					mu.Lock()
					errors++
					mu.Unlock()
					break
				}
				for {
					line, err := reader.ReadString('\n')
					if err != nil {
						mu.Lock()
						errors++
						mu.Unlock()
						break Requests
					}
					line = strings.TrimRight(line, "\r\n")
					if line == "" {
						break
					}
					if strings.HasPrefix(strings.ToLower(line), "content-length:") {
						length, _ = strconv.Atoi(strings.TrimSpace(line[15:]))
					}
				}
				if _, err := reader.Discard(length); err != nil {
					break
				}
				local = append(local, time.Since(start))
			}
			mu.Lock()
			latencies = append(latencies, local...)
			mu.Unlock()
		}()
	}
	wg.Wait()
	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })
	percentile := func(p float64) float64 {
		if len(latencies) == 0 {
			return 0
		}
		return float64(latencies[int(p*float64(len(latencies)-1))].Microseconds()) / 1000
	}
	fmt.Printf("%.0f %.3f %.3f %d\n", float64(len(latencies))/seconds, percentile(0.5), percentile(0.99), errors)
}
