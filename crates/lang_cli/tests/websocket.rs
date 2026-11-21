//! The milestone 9 exit test: `tests/http/chat.vi` (the chat hub from docs/05, plus an echo
//! endpoint) driven by a hand-written WebSocket client.
//!
//! The roadmap's bar is the Autobahn test suite. Fetching it is a download, so this test covers
//! the same ground by its section numbers: framing (1, 2), pings and pongs (2), reserved bits (3),
//! opcodes (4), fragmentation (5), UTF-8 handling (6), the closing handshake and close codes (7),
//! and limits (9). Each case checks what Autobahn checks: the echoed data, or that the server
//! fails the connection with the right close code and then closes TCP.

mod common;

use common::{build_runtime, connect, read_response, start};
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
/// The accept value for `KEY` given in RFC 6455, section 1.3.
const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";

const TEXT: u8 = 0x1;
const BINARY: u8 = 0x2;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

#[derive(Debug, PartialEq)]
struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

impl Frame {
    fn close_code(&self) -> Option<u16> {
        assert_eq!(self.opcode, CLOSE, "expected a close frame, got {self:?}");
        (self.payload.len() >= 2).then(|| u16::from_be_bytes([self.payload[0], self.payload[1]]))
    }
}

fn handshake_request(path: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\nHost: test\r\nUser-Agent: wstester\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
}

/// The bytes of a client frame: masked unless `masked` is false, with `rsv` in bits 4 to 6.
fn encode(fin: bool, rsv: u8, opcode: u8, payload: &[u8], masked: bool) -> Vec<u8> {
    let mut out = vec![(if fin { 0x80 } else { 0 }) | (rsv << 4) | opcode];
    let mask_bit = if masked { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(mask_bit | n as u8),
        n if n <= 0xFFFF => {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    if masked {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    } else {
        out.extend_from_slice(payload);
    }
    out
}

impl Client {
    fn open(port: u16, path: &str) -> Client {
        Client::open_with(port, path, &[])
    }

    /// Connect, send the handshake followed by `early` bytes in the same write, and check the
    /// server's 101 response.
    fn open_with(port: u16, path: &str, early: &[u8]) -> Client {
        let (mut stream, mut reader) = connect(port);
        let mut request = handshake_request(path).into_bytes();
        request.extend_from_slice(early);
        stream.write_all(&request).unwrap();
        let response = read_response(&mut reader, true);
        assert_eq!(response.status, 101, "handshake for {path}: {}", response.body);
        assert_eq!(response.header("sec-websocket-accept"), Some(ACCEPT));
        assert_eq!(response.header("upgrade").map(str::to_ascii_lowercase).as_deref(), Some("websocket"));
        Client { stream, reader }
    }

    fn send(&mut self, opcode: u8, payload: &[u8]) {
        self.raw(&encode(true, 0, opcode, payload, true));
    }

    fn raw(&mut self, bytes: &[u8]) {
        // The server may already have closed the connection in the failure cases.
        let _ = self.stream.write_all(bytes);
    }

    fn read_frame(&mut self) -> Frame {
        let mut head = [0u8; 2];
        self.reader.read_exact(&mut head).expect("expected a frame");
        assert_eq!(head[0] & 0x70, 0, "the server set reserved bits");
        assert_eq!(head[1] & 0x80, 0, "the server masked a frame");
        let length = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                self.reader.read_exact(&mut b).unwrap();
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                self.reader.read_exact(&mut b).unwrap();
                u64::from_be_bytes(b) as usize
            }
            n => n as usize,
        };
        let mut payload = vec![0u8; length];
        self.reader.read_exact(&mut payload).unwrap();
        Frame { fin: head[0] & 0x80 != 0, opcode: head[0] & 0x0F, payload }
    }

    /// Expect one message back, unfragmented.
    fn expect(&mut self, opcode: u8, payload: &[u8]) {
        let frame = self.read_frame();
        assert_eq!((frame.fin, frame.opcode), (true, opcode), "frame {frame:?}");
        assert!(
            frame.payload == payload,
            "payload of {} bytes differs from the {} sent",
            frame.payload.len(),
            payload.len()
        );
    }

    /// Expect the server to fail or close the connection with `code`, then close TCP.
    fn expect_close(&mut self, code: Option<u16>) {
        let frame = self.read_frame();
        assert_eq!(frame.close_code(), code, "close frame {frame:?}");
        self.send(CLOSE, &frame.payload[..frame.payload.len().min(2)]);
        self.expect_eof();
    }

    fn expect_eof(&mut self) {
        let mut rest = Vec::new();
        match self.reader.read_to_end(&mut rest) {
            Ok(_) => assert!(rest.is_empty(), "unexpected bytes after close: {rest:?}"),
            Err(e) => assert!(e.kind() == std::io::ErrorKind::ConnectionReset, "connection not closed: {e}"),
        }
    }

    /// The client starts the closing handshake with `payload` and expects `reply` back.
    fn close_with(&mut self, payload: &[u8], reply: Option<u16>) {
        self.send(CLOSE, payload);
        let frame = self.read_frame();
        assert_eq!(frame.close_code(), reply, "reply to close {payload:?}");
        self.expect_eof();
    }
}

fn close_payload(code: u16, reason: &[u8]) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason);
    payload
}

#[test]
fn chat_hub_and_protocol_conformance() {
    build_runtime();
    let mut server = start("chat");
    let port = server.port;

    // 1. Framing: text and binary echoed at the length boundaries of the frame header.
    let mut echo = Client::open(port, "/echo");
    for length in [0, 1, 125, 126, 127, 65535, 65536, 300_000] {
        let text: Vec<u8> = (0..length).map(|i| b'a' + (i % 26) as u8).collect();
        echo.send(TEXT, &text);
        echo.expect(TEXT, &text);
        let binary: Vec<u8> = (0..length).map(|i| (i * 7 % 256) as u8).collect();
        echo.send(BINARY, &binary);
        echo.expect(BINARY, &binary);
    }

    // 2. Pings are answered with the same payload; unsolicited pongs are ignored.
    echo.send(PING, b"are you there");
    echo.expect(PONG, b"are you there");
    echo.send(PING, &[0xAB; 125]);
    echo.expect(PONG, &[0xAB; 125]);
    echo.send(PONG, b"unsolicited");
    echo.send(TEXT, b"still here");
    echo.expect(TEXT, b"still here");

    // 5. Fragmentation, with a ping in the middle answered before the message completes.
    echo.raw(&encode(false, 0, TEXT, b"frag", true));
    echo.raw(&encode(false, 0, 0, b"men", true));
    echo.send(PING, b"mid");
    echo.raw(&encode(true, 0, 0, b"ted", true));
    echo.expect(PONG, b"mid");
    echo.expect(TEXT, b"fragmented");
    echo.raw(&encode(false, 0, BINARY, &[1, 2], true));
    echo.raw(&encode(true, 0, 0, &[3], true));
    echo.expect(BINARY, &[1, 2, 3]);
    // 6. A multi-byte character split across fragments is fine; the whole message is checked.
    let snowman = "a☃b".as_bytes();
    echo.raw(&encode(false, 0, TEXT, &snowman[..2], true));
    echo.raw(&encode(true, 0, 0, &snowman[2..], true));
    echo.expect(TEXT, snowman);
    // 7. A normal close is echoed, and the server closes TCP.
    echo.close_with(&close_payload(1000, b"bye"), Some(1000));

    // Frames sent right behind the handshake, in the same packet, are not lost.
    let mut early = Client::open_with(port, "/echo", &encode(true, 0, TEXT, b"early", true));
    early.expect(TEXT, b"early");
    early.close_with(&[], Some(1000));

    // Protocol violations: each fails the connection with the code Autobahn expects.
    let failures: Vec<(&str, Vec<u8>, u16)> = vec![
        ("3.1 reserved bit", encode(true, 1, TEXT, b"x", true), 1002),
        ("3.7 reserved bits on a close", encode(true, 7, CLOSE, b"", true), 1002),
        ("4.1.1 reserved opcode 3", encode(true, 0, 3, b"", true), 1002),
        ("4.2.1 reserved opcode 11", encode(true, 0, 11, b"", true), 1002),
        ("2.5 ping too long", encode(true, 0, PING, &[0; 126], true), 1002),
        ("5.1 fragmented ping", encode(false, 0, PING, b"x", true), 1002),
        ("5.9 continuation with nothing to continue", encode(true, 0, 0, b"x", true), 1002),
        ("unmasked client frame", encode(true, 0, TEXT, b"x", false), 1002),
        (
            "6.3.1 invalid UTF-8",
            encode(
                true,
                0,
                TEXT,
                &[0xCE, 0xBA, 0xE1, 0xBD, 0xB9, 0xCF, 0x83, 0xCE, 0xBC, 0xCE, 0xB5, 0xED, 0xA0, 0x80],
                true,
            ),
            1007,
        ),
        ("7.3.2 close with a one-byte payload", encode(true, 0, CLOSE, &[3], true), 1002),
        ("7.9.1 close code 0", encode(true, 0, CLOSE, &close_payload(0, b""), true), 1002),
        ("7.9.2 close code 999", encode(true, 0, CLOSE, &close_payload(999, b""), true), 1002),
        ("7.9.3 close code 1005", encode(true, 0, CLOSE, &close_payload(1005, b""), true), 1002),
        ("7.9.8 close code 2000", encode(true, 0, CLOSE, &close_payload(2000, b""), true), 1002),
        ("7.5.1 close reason not UTF-8", encode(true, 0, CLOSE, &close_payload(1000, &[0xFF, 0xFE]), true), 1007),
        ("9 message over the limit", encode(true, 0, BINARY, &vec![0; 1_048_577], true), 1009),
    ];
    for (case, bytes, code) in failures {
        let mut client = Client::open(port, "/echo");
        client.raw(&bytes);
        let frame = client.read_frame();
        assert_eq!(frame.close_code(), Some(code), "case {case}: {frame:?}");
        client.send(CLOSE, &code.to_be_bytes());
        client.expect_eof();
    }

    // 5.18 A new message started inside a fragmented one, and 9: a fragmented message over the limit.
    let mut client = Client::open(port, "/echo");
    client.raw(&encode(false, 0, TEXT, b"one", true));
    client.raw(&encode(true, 0, TEXT, b"two", true));
    client.expect_close(Some(1002));
    let mut client = Client::open(port, "/echo");
    for opcode in [BINARY, 0, 0] {
        client.raw(&encode(false, 0, opcode, &vec![0; 400_000], true));
    }
    client.expect_close(Some(1009));

    // 7.7 Valid close codes are echoed; an empty close gets a plain 1000.
    for code in [1001, 1002, 1003, 1007, 1011, 3000, 3999, 4000, 4999] {
        Client::open(port, "/echo").close_with(&close_payload(code, b"reason"), Some(code));
    }
    Client::open(port, "/echo").close_with(&[], Some(1000));

    // The upgrade request is still available to the handler: path, query and headers. The
    // server closes with a code and reason of its own.
    let mut who = Client::open(port, "/whoami?x=42");
    who.expect(TEXT, b"/whoami 42 wstester");
    let frame = who.read_frame();
    assert_eq!((frame.close_code(), &frame.payload[2..]), (Some(4000), &b"done"[..]));
    who.send(CLOSE, &frame.payload[..2]);
    who.expect_eof();

    // Handshakes that are not acceptable.
    let (mut stream, mut reader) = connect(port);
    stream.write_all(b"GET /echo HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut reader, false).status, 400);
    let (mut stream, mut reader) = connect(port);
    let old = handshake_request("/echo").replace("Version: 13", "Version: 8");
    stream.write_all(old.as_bytes()).unwrap();
    let response = read_response(&mut reader, false);
    assert_eq!((response.status, response.header("sec-websocket-version")), (426, Some("13")));
    let (mut stream, mut reader) = connect(port);
    let no_key = handshake_request("/echo").replace(&format!("Sec-WebSocket-Key: {KEY}\r\n"), "");
    stream.write_all(no_key.as_bytes()).unwrap();
    assert_eq!(read_response(&mut reader, false).status, 400);
    // Plain HTTP routes still work beside WebSocket ones.
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: t\r\n\r\n").unwrap();
    assert_eq!(read_response(&mut reader, false).body, "ok");

    // The chat hub: joins, broadcasts and leaves reach everyone in the room.
    let mut ann = Client::open(port, "/chat/lobby?name=ann");
    ann.expect(TEXT, b"[lobby] ann joined");
    let mut bob = Client::open(port, "/chat/lobby?name=bob");
    bob.expect(TEXT, b"[lobby] bob joined");
    ann.expect(TEXT, b"[lobby] bob joined");
    ann.send(TEXT, b"hello");
    ann.expect(TEXT, b"[lobby] ann: hello");
    bob.expect(TEXT, b"[lobby] ann: hello");
    // The hub only takes text: binary gets close code 1003.
    bob.send(BINARY, &[1, 2, 3]);
    bob.expect_close(Some(1003));
    ann.expect(TEXT, b"[lobby] bob left");
    ann.close_with(&close_payload(1000, b""), Some(1000));

    // Many clients at once, each with its own conversation.
    let clients: Vec<_> = (0..40)
        .map(|n| {
            std::thread::spawn(move || {
                let mut client = Client::open(port, "/echo");
                for i in 0..25 {
                    let message = format!("client {n} message {i}");
                    client.send(TEXT, message.as_bytes());
                    client.expect(TEXT, message.as_bytes());
                }
                client.close_with(&[], Some(1000));
            })
        })
        .collect();
    for client in clients {
        client.join().unwrap();
    }

    // Shutting down: an open WebSocket is told the server is going away (1001).
    let mut open = Client::open(port, "/echo");
    open.stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    open.send(TEXT, b"ping");
    open.expect(TEXT, b"ping");
    let status = std::process::Command::new("kill").args(["-TERM", &server.child.id().to_string()]).status().unwrap();
    assert!(status.success());
    open.expect_close(Some(1001));
    let exit = server.child.wait().unwrap();
    assert!(exit.success(), "the chat server did not shut down cleanly: {exit}");
}
