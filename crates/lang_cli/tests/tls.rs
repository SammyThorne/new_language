//! HTTPS and secure WebSockets: `tests/http/tls.wb` driven by a rustls client that trusts the
//! test authority in `tests/data/tls`.

mod common;

use common::{build_runtime, repo_root, start};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

type Tls = BufReader<StreamOwned<ClientConnection, TcpStream>>;

fn open(port: u16) -> Tls {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(repo_root().join("tests/data/tls/ca.pem")).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let mut config = ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let name = ServerName::try_from("localhost").unwrap();
    let connection = ClientConnection::new(Arc::new(config), name).unwrap();
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    BufReader::new(StreamOwned::new(connection, stream))
}

/// Status and body of one response.
fn response(tls: &mut Tls) -> (u16, Vec<u8>) {
    let mut line = String::new();
    tls.read_line(&mut line).unwrap();
    let status = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut length = 0;
    loop {
        let mut header = String::new();
        tls.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    tls.read_exact(&mut body).unwrap();
    (status, body)
}

fn send(tls: &mut Tls, bytes: &[u8]) {
    tls.get_mut().write_all(bytes).unwrap();
    tls.get_mut().flush().unwrap();
}

fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n < 65536 => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    let mask = [1, 2, 3, 4];
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    out
}

fn read_frame(tls: &mut Tls) -> (u8, Vec<u8>) {
    let mut head = [0u8; 2];
    tls.read_exact(&mut head).unwrap();
    let length = match head[1] & 0x7F {
        126 => {
            let mut b = [0; 2];
            tls.read_exact(&mut b).unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0; 8];
            tls.read_exact(&mut b).unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut payload = vec![0; length];
    tls.read_exact(&mut payload).unwrap();
    (head[0] & 0x0F, payload)
}

#[test]
fn https_and_secure_websockets() {
    build_runtime();
    let mut server = start("tls");
    let port = server.port;

    // Keep-alive: two requests on one TLS connection, then a large body each way.
    let mut tls = open(port);
    for name in ["ann", "bob"] {
        send(&mut tls, format!("GET /hello?name={name} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes());
        assert_eq!(response(&mut tls), (200, format!("hello {name}").into_bytes()));
    }
    assert_eq!(tls.get_ref().conn.alpn_protocol(), Some(&b"http/1.1"[..]));
    let big: Vec<u8> = (0..1 << 20).map(|i| b'a' + (i % 26) as u8).collect();
    let mut request =
        format!("POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n", big.len()).into_bytes();
    request.extend_from_slice(&big);
    send(&mut tls, &request);
    assert_eq!(response(&mut tls), (200, big.clone()));

    // A client speaking plain HTTP to the TLS port is turned away, and the server carries on.
    let mut plain = TcpStream::connect(("127.0.0.1", port)).unwrap();
    plain.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    plain.write_all(b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut answer = Vec::new();
    let _ = plain.read_to_end(&mut answer);
    assert!(!answer.starts_with(b"HTTP/"), "plain HTTP was answered: {:?}", String::from_utf8_lossy(&answer));

    // A secure WebSocket: text, then a binary message larger than a TLS record, then close.
    let mut ws = open(port);
    send(
        &mut ws,
        b"GET /echo HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
          Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
    );
    let (status, _) = response(&mut ws);
    assert_eq!(status, 101);
    send(&mut ws, &frame(0x1, b"over tls"));
    assert_eq!(read_frame(&mut ws), (0x1, b"over tls".to_vec()));
    send(&mut ws, &frame(0x2, &big[..100_000]));
    assert_eq!(read_frame(&mut ws), (0x2, big[..100_000].to_vec()));
    send(&mut ws, &frame(0x8, &1000u16.to_be_bytes()));
    assert_eq!(read_frame(&mut ws), (0x8, 1000u16.to_be_bytes().to_vec()));

    // The first connection is still usable after all that.
    send(&mut tls, b"GET /hello?name=cy HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(response(&mut tls), (200, b"hello cy".to_vec()));
    server.stop("tls");
}
