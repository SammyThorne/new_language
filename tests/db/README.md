# Database tests

`database.wb` exercises the `postgres` module against a real server. `cargo test` runs it only
when `WEBY_TEST_DATABASE_URL` names a database it may write to (it drops and creates the tables
`users` and `hits`). The URL must include a user and a password, because the test also checks
that a wrong password is refused.

Setting `WEBY_TEST_DATABASE_TLS_URL` as well checks `sslmode=verify-full`. The server then needs
TLS with the test certificate in `tests/data/tls`, and the test needs `SSL_CERT_FILE` set to
`tests/data/tls/ca.pem`.

## A throwaway server

With PostgreSQL's programs on `PATH`, or `PG` set to the directory holding them (on Debian,
`/usr/lib/postgresql/17/bin`):

```bash
dir=$(mktemp -d)
echo secret > "$dir/pw"
"$PG/initdb" -D "$dir/data" -U weby --pwfile="$dir/pw" --auth=scram-sha-256 -E UTF8 --locale=C.UTF-8
cp tests/data/tls/cert.pem "$dir/data/server.crt"
install -m 600 tests/data/tls/key.pem "$dir/data/server.key"
"$PG/pg_ctl" -D "$dir/data" -l "$dir/log" -o "-p 54329 -k '' -c listen_addresses=127.0.0.1 -c ssl=on" start
```

Without root, the Debian packages can be unpacked instead of installed:
`apt-get download postgresql-17 postgresql-client-17 libpq5`, then `dpkg -x` each into one
directory, and `PG` is its `usr/lib/postgresql/17/bin`.

Then:

```bash
WEBY_TEST_DATABASE_URL=postgres://weby:secret@127.0.0.1:54329/postgres \
WEBY_TEST_DATABASE_TLS_URL='postgres://weby:secret@localhost:54329/postgres?sslmode=verify-full' \
SSL_CERT_FILE=tests/data/tls/ca.pem \
cargo test -p lang_cli --test postgres
```

Stop it with `"$PG/pg_ctl" -D "$dir/data" stop`.
