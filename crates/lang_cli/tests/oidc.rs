//! Sign-in with Google against a fake Google that signs its ID tokens (docs/13, milestone 4).
//! The fake runs here, on a free port, with the test-only keys in `tests/fixtures/`; the
//! program `tests/oidc/signin` runs its app and drives a browser's requests through both, and
//! its output must equal its `# expect:` lines.
//!
//! The fake's routes:
//! - `GET /authorize`: signs in at once as the account `login_hint` names, redirecting back with
//!   a code that carries the nonce and the PKCE challenge.
//! - `POST /token`: checks the PKCE verifier and answers with an ID token, signed or spoiled as
//!   the account's name says.
//! - `GET /keys`: the JSON Web Key Set, after a pause, so that sign-ins arriving together would
//!   fetch it more than once if the provider did not hold them back. `GET /fetches` counts them.
//! - `GET /rotate`: starts signing with the second key, and publishes it next to the first.
//!
//! The example in `examples/google_login` is checked here too.

mod common;

use common::{build_runtime, compiler, repo_root};
use lang_names::SOURCE_EXTENSION;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair, RsaPublicKeyComponents};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CLIENT: &str = "test-client.apps.googleusercontent.com";

struct Fake {
    /// The keys, with their IDs; those up to `current` are published.
    keys: Vec<(String, RsaKeyPair)>,
    current: AtomicUsize,
    fetches: AtomicUsize,
}

#[test]
fn sign_in_with_signed_tokens() {
    build_runtime();
    let root = repo_root();
    let keys = ["oidc_test_key.pem", "oidc_test_key_2.pem"]
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let pem = std::fs::read_to_string(root.join("tests/fixtures").join(file)).unwrap();
            (format!("test-key-{}", index + 1), RsaKeyPair::from_pkcs8(&pem_der(&pem)).unwrap())
        })
        .collect();
    let fake = Arc::new(Fake { keys, current: AtomicUsize::new(0), fetches: AtomicUsize::new(0) });
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let fake = Arc::clone(&fake);
            std::thread::spawn(move || serve(&fake, stream));
        }
    });

    let program = root.join(format!("tests/oidc/signin.{SOURCE_EXTENSION}"));
    let source = std::fs::read_to_string(&program).unwrap();
    let expected: String =
        source.lines().filter_map(|l| l.strip_prefix("# expect: ")).map(|l| format!("{l}\n")).collect();
    let output = Command::new(compiler())
        .arg("run")
        .arg(&program)
        .arg(format!("http://127.0.0.1:{port}"))
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout == expected,
        "{}\n--- expected\n{expected}--- actual\n{stdout}--- stderr\n{stderr}",
        output.status
    );
    for reason in
        ["its signature is not valid", "its algorithm is \"none\"", "its algorithm is \"HS256\"", "its key 'retired'"]
    {
        assert!(stderr.contains(reason), "the log does not give the reason {reason:?}:\n{stderr}");
    }
}

/// The example app in `examples/google_login` builds, and its tests pass. Running it needs a
/// real Google client, so that is checked by hand (docs/13, milestone 6).
#[test]
fn the_google_login_example_passes_its_tests() {
    build_runtime();
    let root = repo_root();
    let example = root.join("examples/google_login");
    let check = Command::new(compiler())
        .arg("check")
        .arg(format!("main.{SOURCE_EXTENSION}"))
        .current_dir(&example)
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    let test = Command::new(compiler()).arg("test").current_dir(&example).output().unwrap();
    let stdout = String::from_utf8_lossy(&test.stdout);
    assert!(
        test.status.success() && stdout.contains("5 passed; 0 failed"),
        "{stdout}{}",
        String::from_utf8_lossy(&test.stderr)
    );
}

/// One request, answered, and the connection closed.
fn serve(fake: &Fake, stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let target = request_line.split(' ').nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, headers, body) = match path {
        "/authorize" => authorize(&form(query)),
        "/token" => token(fake, &form(&String::from_utf8(body).unwrap())),
        "/keys" => {
            std::thread::sleep(Duration::from_millis(300));
            fake.fetches.fetch_add(1, Ordering::SeqCst);
            (200, "Cache-Control: public, max-age=21600, must-revalidate, no-transform\r\n".to_string(), key_set(fake))
        }
        "/fetches" => (200, String::new(), fake.fetches.load(Ordering::SeqCst).to_string()),
        "/rotate" => {
            fake.current.store(1, Ordering::SeqCst);
            (200, String::new(), "rotated".to_string())
        }
        _ => (404, String::new(), "{}".to_string()),
    };
    let reason = if status == 302 { "Found" } else { "OK" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    let mut stream = stream;
    let _ = stream.write_all(response.as_bytes());
}

fn authorize(query: &[(String, String)]) -> (u16, String, String) {
    let get = |name: &str| query.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or("");
    let code = format!("{}.{}.{}", get("login_hint"), get("nonce"), get("code_challenge"));
    let location = format!("{}?code={code}&state={}", get("redirect_uri"), get("state"));
    (302, format!("Location: {location}\r\n"), String::new())
}

fn token(fake: &Fake, fields: &[(String, String)]) -> (u16, String, String) {
    let get = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str()).unwrap_or("");
    let code: Vec<&str> = get("code").split('.').collect();
    let challenge = base64url(ring::digest::digest(&ring::digest::SHA256, get("code_verifier").as_bytes()).as_ref());
    if code.len() != 3 || code[2] != challenge || get("client_secret") != "test-secret" || get("client_id") != CLIENT {
        return (400, String::new(), r#"{"error":"invalid_grant"}"#.to_string());
    }
    let (scenario, nonce) = (code[0], code[1]);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = |name: &str| {
        format!(
            r#"{{"iss":"https://accounts.google.com","azp":"{CLIENT}","aud":"{CLIENT}","sub":"1001","email":"ann@example.com","email_verified":true,"name":"{name}","iat":{now},"exp":{},"nonce":"{nonce}"}}"#,
            now + 3600
        )
    };
    let current = fake.current.load(Ordering::SeqCst);
    let (kid, key) = (&fake.keys[current].0, &fake.keys[current].1);
    let other = &fake.keys[1 - current].1;
    let header = |alg: &str, kid: &str| base64url(format!(r#"{{"alg":"{alg}","kid":"{kid}","typ":"JWT"}}"#).as_bytes());
    let payload = base64url(claims("Ann Lee").as_bytes());
    let id_token = match scenario {
        "tampered" => {
            let signed = signed(key, &header("RS256", kid), &payload);
            let signature = signed.rsplit('.').next().unwrap();
            format!("{}.{}.{signature}", header("RS256", kid), base64url(claims("Mallory").as_bytes()))
        }
        "none" => format!("{}.{payload}.", header("none", kid)),
        "hs256" => format!("{}.{payload}.{}", header("HS256", kid), base64url(&[0; 32])),
        "unknown_kid" => signed(key, &header("RS256", "retired"), &payload),
        "wrong_key" => signed(other, &header("RS256", kid), &payload),
        _ => signed(key, &header("RS256", kid), &payload),
    };
    let body = format!(
        r#"{{"access_token":"ya29.access","expires_in":3599,"id_token":"{id_token}","scope":"openid email profile","token_type":"Bearer"}}"#
    );
    (200, String::new(), body)
}

/// `header.payload.signature`, signed with RS256.
fn signed(key: &RsaKeyPair, header: &str, payload: &str) -> String {
    let message = format!("{header}.{payload}");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), message.as_bytes(), &mut signature).unwrap();
    format!("{message}.{}", base64url(&signature))
}

fn key_set(fake: &Fake) -> String {
    let published = &fake.keys[..=fake.current.load(Ordering::SeqCst)];
    let keys: Vec<String> = published
        .iter()
        .map(|(kid, key)| {
            let parts = RsaPublicKeyComponents::<Vec<u8>>::from(key.public());
            format!(
                r#"{{"kty":"RSA","alg":"RS256","use":"sig","kid":"{kid}","n":"{}","e":"{}"}}"#,
                base64url(&parts.n),
                base64url(&parts.e)
            )
        })
        .collect();
    format!(r#"{{"keys":[{}]}}"#, keys.join(","))
}

/// `a=1&b=2`, decoded.
fn form(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(name), percent_decode(value))
        })
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// The DER inside a PEM file's `-----BEGIN ...-----` and `-----END ...-----` lines.
fn pem_der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .skip_while(|l| !l.starts_with("-----BEGIN"))
        .skip(1)
        .take_while(|l| !l.starts_with("-----END"))
        .collect();
    let mut out = Vec::new();
    let (mut bits, mut count) = (0u32, 0);
    for c in body.bytes().filter(|&c| c != b'=') {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("not base64: {c}"),
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    out
}
