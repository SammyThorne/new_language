// Open N WebSocket connections to the benchmark server, report when all are open, wait for a
// line on standard input, then send one message on every connection and check the echo.
//
// Usage: client <port> <connections>
//
// Connections come from several loopback source addresses (127.0.0.2, 127.0.0.3, ...), 20,000
// each, so that the client does not run out of ephemeral ports.
package main

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
)

const handshake = "GET /ws HTTP/1.1\r\nHost: bench\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"

func open(port int, source string) (net.Conn, error) {
	dialer := net.Dialer{LocalAddr: &net.TCPAddr{IP: net.ParseIP(source)}}
	conn, err := dialer.Dial("tcp", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return nil, err
	}
	if _, err := conn.Write([]byte(handshake)); err != nil {
		return nil, err
	}
	reader := bufio.NewReader(conn)
	status, err := reader.ReadString('\n')
	if err != nil || !strings.Contains(status, " 101 ") {
		return nil, fmt.Errorf("handshake failed: %q %v", status, err)
	}
	for {
		line, err := reader.ReadString('\n')
		if err != nil {
			return nil, err
		}
		if line == "\r\n" {
			break
		}
	}
	return conn, nil
}

// A masked text frame, as a client must send.
func frame(text string) []byte {
	mask := []byte{1, 2, 3, 4}
	out := []byte{0x81, 0x80 | byte(len(text))}
	out = append(out, mask...)
	for i := 0; i < len(text); i++ {
		out = append(out, text[i]^mask[i%4])
	}
	return out
}

func main() {
	port, _ := strconv.Atoi(os.Args[1])
	count, _ := strconv.Atoi(os.Args[2])
	conns := make([]net.Conn, count)
	var failed atomic.Int64
	var wg sync.WaitGroup
	next := atomic.Int64{}
	for w := 0; w < 64; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for {
				i := int(next.Add(1) - 1)
				if i >= count {
					return
				}
				source := fmt.Sprintf("127.0.0.%d", 2+i/20000)
				conn, err := open(port, source)
				if err != nil {
					if failed.Add(1) == 1 {
						fmt.Fprintln(os.Stderr, "open:", err)
					}
					continue
				}
				conns[i] = conn
			}
		}()
	}
	wg.Wait()
	fmt.Printf("connected %d failed %d\n", int64(count)-failed.Load(), failed.Load())
	bufio.NewReader(os.Stdin).ReadString('\n')

	var echoed atomic.Int64
	next.Store(0)
	for w := 0; w < 64; w++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			buf := make([]byte, 64)
			for {
				i := int(next.Add(1) - 1)
				if i >= count {
					return
				}
				conn := conns[i]
				if conn == nil {
					continue
				}
				message := fmt.Sprintf("hi %d", i)
				if _, err := conn.Write(frame(message)); err != nil {
					continue
				}
				n, err := conn.Read(buf)
				if err == nil && n >= 2 && string(buf[2:n]) == message {
					echoed.Add(1)
				}
			}
		}()
	}
	wg.Wait()
	fmt.Printf("echoed %d\n", echoed.Load())
}
