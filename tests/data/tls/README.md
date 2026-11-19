# Test certificates

For the TLS tests only; the private key is public, so never trust `ca.pem` outside them.

- `ca.pem`: a self-signed authority, "Weby test CA" (its key was thrown away).
- `cert.pem`, `key.pem`: a server certificate for `localhost` only (not `127.0.0.1`, so the
  tests can check that a name mismatch is refused), signed by that authority, valid until 2126.
