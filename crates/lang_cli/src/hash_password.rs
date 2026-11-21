//! `hash-password`: turn a password into the `METRICS_PASSWORD_HASH` form that `server.metrics`
//! accepts, `pbkdf2-sha256$<iterations>$<salt>$<hash>`, with the salt and hash in standard
//! base64. The password is read from the terminal without echo, or from standard input when
//! that is not a terminal (one line, without its line ending), so it never appears in the
//! shell's history or the process list.

use std::io::{self, BufRead, IsTerminal, Write};
use std::num::NonZeroU32;
use std::process::{Command, Stdio};

use ring::rand::{SecureRandom, SystemRandom};

/// OWASP's 2023 figure for PBKDF2 with SHA-256.
pub const DEFAULT_ITERATIONS: u32 = 600_000;

/// Hash `password` with a fresh 16-byte salt.
pub fn hash(password: &str, iterations: NonZeroU32) -> Result<String, String> {
    let mut salt = [0u8; 16];
    SystemRandom::new().fill(&mut salt).map_err(|_| "the system's random number generator failed".to_string())?;
    Ok(hash_with_salt(password, iterations, &salt))
}

/// The encoded hash of `password` with `salt`.
pub fn hash_with_salt(password: &str, iterations: NonZeroU32, salt: &[u8]) -> String {
    let mut out = [0u8; 32];
    ring::pbkdf2::derive(ring::pbkdf2::PBKDF2_HMAC_SHA256, iterations, salt, password.as_bytes(), &mut out);
    format!("pbkdf2-sha256${iterations}${}${}", base64(salt), base64(&out))
}

/// Standard base64, with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// Read the password: twice, without echo, from a terminal; once from a pipe.
pub fn read_password() -> Result<String, String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).map_err(|error| format!("cannot read the password: {error}"))?;
        let password = line.strip_suffix('\n').unwrap_or(&line);
        let password = password.strip_suffix('\r').unwrap_or(password);
        return Ok(password.to_string());
    }
    let first = prompt("Password: ")?;
    let second = prompt("Again: ")?;
    if first != second {
        return Err("the passwords differ".to_string());
    }
    Ok(first)
}

/// Prompt on standard error and read a line with the terminal's echo off.
fn prompt(text: &str) -> Result<String, String> {
    eprint!("{text}");
    let _ = io::stderr().flush();
    let echo = |on: bool| {
        Command::new("stty")
            .arg(if on { "echo" } else { "-echo" })
            .stdin(Stdio::inherit())
            .status()
            .is_ok_and(|s| s.success())
    };
    let hidden = echo(false);
    let mut line = String::new();
    let read = io::stdin().lock().read_line(&mut line);
    if hidden {
        echo(true);
    }
    eprintln!();
    read.map_err(|error| format!("cannot read the password: {error}"))?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_a_known_vector() {
        // PBKDF2-HMAC-SHA256("password", "salt", 1), as Python's hashlib.pbkdf2_hmac gives it.
        let encoded = hash_with_salt("password", NonZeroU32::new(1).unwrap(), b"salt");
        assert_eq!(encoded, "pbkdf2-sha256$1$c2FsdA==$Eg+2z/z4syxD5yJSVsT4N6hlSMkszDVICAWYfLcL4Xs=");
    }

    #[test]
    fn base64_pads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn fresh_salts_differ() {
        let one = NonZeroU32::new(1).unwrap();
        assert_ne!(hash("x", one).unwrap(), hash("x", one).unwrap());
    }
}
