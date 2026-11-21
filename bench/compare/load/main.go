// The load generator for bench/compare. Standard library only; it speaks HTTP/1.1 keep-alive and
// WebSocket over raw TCP so that it adds as little cost as possible.
//
//	load http PORT METHOD PATH CONNS SECONDS [RATE [BODY]]
//	    CONNS keep-alive connections send requests for SECONDS. RATE 0 sends each request as soon
//	    as the previous answer arrives (closed loop: measures the most the server can do); RATE > 0
//	    spreads that many requests a second over the connections (open loop), and measures each
//	    latency from when the request was due, so a server that falls behind shows its queueing.
//	load ws-echo PORT CONNS SECONDS SIZE
//	    CONNS WebSocket connections each send a SIZE-byte text message and wait for its echo.
//	load ws-idle PORT CONNS
//	    Opens CONNS WebSocket connections, checks each with one echo, prints "ready", holds them
//	    until standard input closes, then closes them.
//
// The first three modes print one JSON object: rate, latency percentiles in milliseconds, errors.
package main

import (
	"bufio"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type result struct {
	Rate     float64 `json:"rate"`
	Count    int     `json:"count"`
	Errors   int64   `json:"errors"`
	P50      float64 `json:"p50"`
	P90      float64 `json:"p90"`
	P99      float64 `json:"p99"`
	P999     float64 `json:"p999"`
	Max      float64 `json:"max"`
	Seconds  float64 `json:"seconds"`
	Wanted   float64 `json:"wanted,omitempty"`
	Statuses string  `json:"statuses,omitempty"`
}

func summarize(all [][]time.Duration, seconds float64, errs int64) result {
	var latencies []time.Duration
	for _, l := range all {
		latencies = append(latencies, l...)
	}
	sort.Slice(latencies, func(i, j int) bool { return latencies[i] < latencies[j] })
	pct := func(p float64) float64 {
		if len(latencies) == 0 {
			return 0
		}
		return float64(latencies[int(p*float64(len(latencies)-1))].Microseconds()) / 1000
	}
	return result{
		Rate: float64(len(latencies)) / seconds, Count: len(latencies), Errors: errs,
		P50: pct(0.5), P90: pct(0.9), P99: pct(0.99), P999: pct(0.999), Max: pct(1), Seconds: seconds,
	}
}

// readResponse reads one HTTP response and returns its status and whether the server will close.
func readResponse(r *bufio.Reader) (int, bool, error) {
	line, err := r.ReadString('\n')
	if err != nil {
		return 0, false, err
	}
	parts := strings.SplitN(line, " ", 3)
	if len(parts) < 2 {
		return 0, false, errors.New("bad status line")
	}
	status, _ := strconv.Atoi(parts[1])
	length, chunked, closing := -1, false, false
	for {
		line, err := r.ReadString('\n')
		if err != nil {
			return 0, false, err
		}
		line = strings.TrimRight(line, "\r\n")
		if line == "" {
			break
		}
		name, value, _ := strings.Cut(line, ":")
		value = strings.TrimSpace(value)
		switch strings.ToLower(name) {
		case "content-length":
			length, _ = strconv.Atoi(value)
		case "transfer-encoding":
			chunked = strings.Contains(strings.ToLower(value), "chunked")
		case "connection":
			closing = strings.EqualFold(value, "close")
		}
	}
	switch {
	case chunked:
		for {
			size, err := r.ReadString('\n')
			if err != nil {
				return 0, false, err
			}
			n, err := strconv.ParseInt(strings.TrimSpace(strings.Split(size, ";")[0]), 16, 64)
			if err != nil {
				return 0, false, err
			}
			if _, err := r.Discard(int(n) + 2); err != nil {
				return 0, false, err
			}
			if n == 0 {
				break
			}
		}
	case length >= 0:
		if _, err := r.Discard(length); err != nil {
			return 0, false, err
		}
	default:
		_, err := io.Copy(io.Discard, r)
		return status, true, err
	}
	return status, closing, nil
}

func loadHTTP(port, method, path string, conns int, seconds, rate float64, body string) result {
	request := method + " " + path + " HTTP/1.1\r\nHost: localhost\r\n"
	if body != "" {
		request += "Content-Type: application/json\r\nContent-Length: " + strconv.Itoa(len(body)) + "\r\n"
	}
	request += "\r\n" + body
	raw := []byte(request)
	// Open every connection before the clock starts, so connecting is not counted as latency.
	opened := make([]net.Conn, conns)
	var dialing sync.WaitGroup
	slots := make(chan struct{}, 128)
	for c := range opened {
		dialing.Add(1)
		slots <- struct{}{}
		go func(c int) {
			defer dialing.Done()
			defer func() { <-slots }()
			if conn, err := net.DialTimeout("tcp", "127.0.0.1:"+port, 10*time.Second); err == nil {
				opened[c] = conn
			}
		}(c)
	}
	dialing.Wait()
	start := time.Now()
	deadline := start.Add(time.Duration(seconds * float64(time.Second)))
	var errs atomic.Int64
	var statusMu sync.Mutex
	statuses := map[int]int{}
	all := make([][]time.Duration, conns)
	var wg sync.WaitGroup
	for c := 0; c < conns; c++ {
		wg.Add(1)
		go func(c int) {
			defer wg.Done()
			local := make([]time.Duration, 0, 1<<16)
			var interval time.Duration
			next := start
			if rate > 0 {
				interval = time.Duration(float64(conns) / rate * float64(time.Second))
				next = start.Add(time.Duration(float64(interval) * float64(c) / float64(conns)))
			}
			// When the connection's last response arrived.
			free := start
			conn := opened[c]
			var reader *bufio.Reader
			if conn != nil {
				conn.SetDeadline(deadline.Add(10 * time.Second))
				reader = bufio.NewReaderSize(conn, 64*1024)
			}
			for {
				due := time.Now()
				if rate > 0 {
					if next.After(deadline) {
						break
					}
					if wait := time.Until(next); wait > 0 {
						time.Sleep(wait)
					}
					// A request delayed because the server had not yet answered the previous one is
					// timed from when it was due, so the wait counts against the server. A request
					// whose connection was already free is timed from when it is sent, so that
					// time.Sleep waking late does not count against the server.
					if free.After(next) {
						due = next
					} else {
						due = time.Now()
					}
					next = next.Add(interval)
				} else if due.After(deadline) {
					break
				}
				if conn == nil {
					var err error
					conn, err = net.DialTimeout("tcp", "127.0.0.1:"+port, 5*time.Second)
					if err != nil {
						errs.Add(1)
						time.Sleep(10 * time.Millisecond)
						continue
					}
					conn.SetDeadline(deadline.Add(10 * time.Second))
					reader = bufio.NewReaderSize(conn, 64*1024)
				}
				if rate == 0 {
					due = time.Now()
				}
				_, err := conn.Write(raw)
				var status int
				var closing bool
				if err == nil {
					status, closing, err = readResponse(reader)
				}
				free = time.Now()
				if err != nil {
					errs.Add(1)
					conn.Close()
					conn = nil
					continue
				}
				if status < 200 || status > 299 {
					errs.Add(1)
					statusMu.Lock()
					statuses[status]++
					statusMu.Unlock()
				} else {
					local = append(local, time.Since(due))
				}
				if closing {
					conn.Close()
					conn = nil
				}
			}
			if conn != nil {
				conn.Close()
			}
			all[c] = local
		}(c)
	}
	wg.Wait()
	elapsed := time.Since(start).Seconds()
	res := summarize(all, elapsed, errs.Load())
	res.Wanted = rate
	if len(statuses) > 0 {
		res.Statuses = fmt.Sprint(statuses)
	}
	return res
}

// ---- WebSocket ----------------------------------------------------------------------------

type wsConn struct {
	conn   net.Conn
	reader *bufio.Reader
	frame  []byte
}

func dialWS(port string, local net.IP) (*wsConn, error) {
	dialer := net.Dialer{Timeout: 10 * time.Second}
	if local != nil {
		dialer.LocalAddr = &net.TCPAddr{IP: local}
	}
	conn, err := dialer.Dial("tcp", "127.0.0.1:"+port)
	if err != nil {
		return nil, err
	}
	conn.SetDeadline(time.Now().Add(30 * time.Second))
	key := make([]byte, 16)
	rand.Read(key)
	handshake := "GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n" +
		"Sec-WebSocket-Key: " + base64.StdEncoding.EncodeToString(key) + "\r\nSec-WebSocket-Version: 13\r\n\r\n"
	if _, err := conn.Write([]byte(handshake)); err != nil {
		conn.Close()
		return nil, err
	}
	reader := bufio.NewReaderSize(conn, 16*1024)
	status, err := reader.ReadString('\n')
	if err != nil || !strings.Contains(status, " 101 ") {
		conn.Close()
		return nil, fmt.Errorf("handshake: %q %v", status, err)
	}
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			conn.Close()
			return nil, err
		}
		if line == "\r\n" {
			break
		}
	}
	conn.SetDeadline(time.Time{})
	return &wsConn{conn: conn, reader: reader}, nil
}

// send writes one masked text frame.
func (w *wsConn) send(payload []byte) error {
	w.frame = w.frame[:0]
	w.frame = append(w.frame, 0x81)
	n := len(payload)
	switch {
	case n < 126:
		w.frame = append(w.frame, 0x80|byte(n))
	case n < 65536:
		w.frame = append(w.frame, 0x80|126, byte(n>>8), byte(n))
	default:
		w.frame = append(w.frame, 0x80|127, 0, 0, 0, 0, byte(n>>24), byte(n>>16), byte(n>>8), byte(n))
	}
	mask := [4]byte{1, 2, 3, 4}
	w.frame = append(w.frame, mask[:]...)
	for i, b := range payload {
		w.frame = append(w.frame, b^mask[i%4])
	}
	_, err := w.conn.Write(w.frame)
	return err
}

// receive reads one unmasked frame and returns its payload length.
func (w *wsConn) receive() (int, error) {
	head := make([]byte, 2)
	if _, err := io.ReadFull(w.reader, head); err != nil {
		return 0, err
	}
	n := int(head[1] & 0x7f)
	switch n {
	case 126:
		ext := make([]byte, 2)
		if _, err := io.ReadFull(w.reader, ext); err != nil {
			return 0, err
		}
		n = int(ext[0])<<8 | int(ext[1])
	case 127:
		ext := make([]byte, 8)
		if _, err := io.ReadFull(w.reader, ext); err != nil {
			return 0, err
		}
		n = 0
		for _, b := range ext {
			n = n<<8 | int(b)
		}
	}
	if head[1]&0x80 != 0 {
		if _, err := w.reader.Discard(4); err != nil {
			return 0, err
		}
	}
	if _, err := w.reader.Discard(n); err != nil {
		return 0, err
	}
	if head[0]&0x0f == 0x9 || head[0]&0x0f == 0xA { // ping or pong: not our echo
		return w.receive()
	}
	return n, nil
}

func wsEcho(port string, conns int, seconds float64, size int) result {
	payload := []byte(strings.Repeat("x", size))
	start := time.Now()
	deadline := start.Add(time.Duration(seconds * float64(time.Second)))
	var errs atomic.Int64
	all := make([][]time.Duration, conns)
	var wg sync.WaitGroup
	for c := 0; c < conns; c++ {
		wg.Add(1)
		go func(c int) {
			defer wg.Done()
			ws, err := dialWS(port, nil)
			if err != nil {
				errs.Add(1)
				return
			}
			defer ws.conn.Close()
			ws.conn.SetDeadline(deadline.Add(10 * time.Second))
			local := make([]time.Duration, 0, 1<<16)
			for time.Now().Before(deadline) {
				sent := time.Now()
				if err := ws.send(payload); err != nil {
					errs.Add(1)
					break
				}
				n, err := ws.receive()
				if err != nil || n != size {
					errs.Add(1)
					break
				}
				local = append(local, time.Since(sent))
			}
			all[c] = local
		}(c)
	}
	wg.Wait()
	return summarize(all, time.Since(start).Seconds(), errs.Load())
}

func wsIdle(port string, conns int) {
	held := make([]*wsConn, conns)
	var failed atomic.Int64
	var wg sync.WaitGroup
	slots := make(chan struct{}, 256)
	for c := 0; c < conns; c++ {
		wg.Add(1)
		slots <- struct{}{}
		go func(c int) {
			defer wg.Done()
			defer func() { <-slots }()
			// Spread the connections over loopback addresses so no one address runs out of ports.
			local := net.IPv4(127, 0, byte(1+c/20000), byte(1+(c/250)%250))
			for attempt := 0; attempt < 3; attempt++ {
				ws, err := dialWS(port, local)
				if err == nil {
					ws.conn.SetDeadline(time.Now().Add(30 * time.Second))
					if ws.send([]byte("hi")) == nil {
						if n, err := ws.receive(); err == nil && n == 2 {
							ws.conn.SetDeadline(time.Time{})
							held[c] = ws
							return
						}
					}
					ws.conn.Close()
				}
				time.Sleep(100 * time.Millisecond)
			}
			failed.Add(1)
		}(c)
	}
	wg.Wait()
	fmt.Printf("ready %d %d\n", int64(conns)-failed.Load(), failed.Load())
	io.Copy(io.Discard, os.Stdin)
	for _, ws := range held {
		if ws != nil {
			ws.conn.Close()
		}
	}
}

func main() {
	args := os.Args[1:]
	if len(args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: load http|ws-echo|ws-idle PORT ...")
		os.Exit(2)
	}
	var res result
	switch args[0] {
	case "http":
		conns, _ := strconv.Atoi(args[4])
		seconds, _ := strconv.ParseFloat(args[5], 64)
		rate := 0.0
		body := ""
		if len(args) > 6 {
			rate, _ = strconv.ParseFloat(args[6], 64)
		}
		if len(args) > 7 {
			body = args[7]
		}
		res = loadHTTP(args[1], args[2], args[3], conns, seconds, rate, body)
	case "ws-echo":
		conns, _ := strconv.Atoi(args[2])
		seconds, _ := strconv.ParseFloat(args[3], 64)
		size, _ := strconv.Atoi(args[4])
		res = wsEcho(args[1], conns, seconds, size)
	case "ws-idle":
		conns, _ := strconv.Atoi(args[2])
		wsIdle(args[1], conns)
		return
	}
	out, _ := json.Marshal(res)
	fmt.Println(string(out))
}
