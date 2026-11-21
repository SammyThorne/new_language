# Metrics dashboard

A small orders service whose metrics, and a dashboard that charts them, are behind a password.
`server.metrics()` serves the metrics in the Prometheus text format at `/metrics`, and the
dashboard at `/metrics/dashboard`: requests per second by status, latency percentiles, requests
in flight, connections, tasks and memory over the last hour; each route's totals; the recent
server errors and the slowest requests, with their request IDs to find in the access log; and the
program's own metrics (`shop_orders_total` and `shop_basket_items` here).

## Run it

1. Make a password hash. The command asks for the password twice, without showing it, and
   prints the hash:

   ```sh
   viper hash-password
   ```

2. Copy `.env.example` to `.env`, set `METRICS_USER`, and paste the hash after
   `METRICS_PASSWORD_HASH=`. (`METRICS_PASSWORD` with the password itself works too, but then
   the password is in the file.)

3. Run the service and open <http://localhost:8080/metrics/dashboard>:

   ```sh
   viper run main.vi
   ```

   Place some orders to see the charts move:

   ```sh
   for i in $(seq 200); do curl -s -X POST "localhost:8080/orders?items=$((i % 7 + 1))" > /dev/null; done
   curl -s localhost:8080/slow localhost:8080/fail
   ```

## Test it

```sh
viper test
```

The tests pass the user name and password to `make_server`, and call the server with
`http.test_request`, so nothing listens on a port.

## In production

- Serve it over HTTPS (`server.listen_tls(...)`, or behind a proxy listed in
  `server.trusted_proxies` that sends `X-Forwarded-Proto: https`). Over
  plain HTTP from another machine the password crosses the network readably, and the server
  says so in its log the first time.
- After ten wrong passwords in a minute a client is refused with `429 Too Many Requests` for a
  minute.
- The page and its data are never cached, may not be framed, and load no script or style from
  anywhere else. Paths are shown without their query strings.
- The history is kept in memory, 720 samples five seconds apart, and starts again when the
  server restarts. For longer history, point Prometheus at `/metrics`.
- `server.metrics(dashboard=False)` serves only `/metrics`.
