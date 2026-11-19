// A small HTTP load generator: `connections` keep-alive connections each send GET requests one
// after another for `seconds`, and the program prints requests per second and latency
// percentiles. Written because no load-testing tool is installed on the benchmark machine.
//
// Usage: load [--browser] PORT PATH CONNECTIONS SECONDS [METHOD BODY]
//
// With METHOD and BODY, each request is that method with BODY as a JSON body. With --browser,
// each request also carries the dozen headers a browser sends (about 600 bytes), so that the
// cost of parsing headers is measured too; without it, requests carry only Host.
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

// The headers a desktop browser sends with a same-site API request, after Host.
const browserHeaders = "User-Agent: Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36\r\n" +
	"Accept: application/json, text/plain, */*\r\n" +
	"Accept-Language: en-GB,en-US;q=0.9,en;q=0.8\r\n" +
	"Accept-Encoding: gzip, deflate, br, zstd\r\n" +
	"Referer: http://localhost/dashboard/orders?page=2\r\n" +
	"Cookie: session=6f1c2a9e8b7d4c3fa0e1d2c3b4a59687; theme=dark; consent=analytics%3Dfalse\r\n" +
	"Cache-Control: no-cache\r\n" +
	"Pragma: no-cache\r\n" +
	"Sec-Fetch-Dest: empty\r\n" +
	"Sec-Fetch-Mode: cors\r\n" +
	"Sec-Fetch-Site: same-origin\r\n" +
	"Connection: keep-alive\r\n"

func main() {
	args := os.Args[1:]
	extra := ""
	if len(args) > 0 && args[0] == "--browser" {
		extra = browserHeaders
		args = args[1:]
	}
	port, path := args[0], args[1]
	connections, _ := strconv.Atoi(args[2])
	seconds, _ := strconv.ParseFloat(args[3], 64)
	request := []byte("GET " + path + " HTTP/1.1\r\nHost: localhost\r\n" + extra + "\r\n")
	if len(args) > 5 {
		body := args[5]
		request = []byte(args[4] + " " + path + " HTTP/1.1\r\nHost: localhost\r\n" + extra + "Content-Type: application/json\r\nContent-Length: " +
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
