# Sign in with Google: research and plan

This is a plan, written so that an agent with no other context can carry it out. **Milestone 1 is built; its decisions (section 5) are confirmed.** Each milestone in section 6 ends with a "Done" note once built. It adds a standard library module, `auth`, that lets a program sign its users in with their Google accounts. The program points it at the credentials file it downloaded from the Google Cloud console. It marks the routes that need a signed-in user with `@auth.required`, or by giving the handler a parameter of type `auth.User`. The module provides the login, callback and logout routes, the session cookie, and the checks of Google's tokens. The program writes none of the OAuth flow itself.

It mirrors what web developers get elsewhere:
- Flask: Flask-Login's `@login_required` on top of Authlib or Flask-Dance.
- Express: Passport's `passport-google-oauth20` strategy.
- Fastify: `@fastify/oauth2`.
- Next.js: Auth.js's Google provider.

Unlike most of them, setting up Google sign-in here is one call, and protecting a route is one line.

Read first:
- [05-standard-library.md](05-standard-library.md): HTTP, "Route annotations", and "How handler parameters are bound".
- `crates/lang_sema/src/check/http.rs`: how routes are checked and how `mount` generates the adapters that bind parameters.
- `stdlib/http.lang`: `Request`, `Response`, `Server`, middleware, and the client.
- [12-openapi-docs-plan.md](12-openapi-docs-plan.md): security schemes in the API documentation.
- The conventions in [11-release-polish-plan.md](11-release-polish-plan.md): commit and push per milestone, the gates, and section "Rules".

## 1. What the user gets

```python
import http
import auth
import os

@derive(Json)
struct Note:
    id: int
    text: str

impl App:
    @http.get("/")
    def home(self, user: auth.User?) -> str:          # optional: works signed in or out
        match user:
            case Some(u):
                return f"Hello, {u.name}"
            case None:
                return "Hello! Sign in at /auth/google/login"

    @http.get("/notes")
    @auth.required                                    # a 401, or a redirect to sign-in
    def notes(self) -> list[Note]:
        ...

    @http.get("/me")
    def me(self, user: auth.User) -> auth.User:       # a required `auth.User`: also protected
        return user

def main():
    server = http.Server()
    google = auth.google(
        "secrets/client_secret.json",                 # downloaded from the Cloud console
        secret=os.getenv("SESSION_SECRET"),           # signs the session cookie
    ).unwrap()
    google.install(server)                            # the login routes and the session middleware
    server.mount(App())
    server.listen(":8080").unwrap()
```

What `google.install(server)` adds:

| Route | What it does |
|---|---|
| `GET /auth/google/login?next=/notes` | Starts sign-in. It sets a short-lived cookie holding the flow's `state`, `nonce` and PKCE verifier, then redirects to Google. |
| `GET /auth/google/callback` | Google's redirect back. It checks `state`, exchanges the code, validates the ID token, sets the session cookie, and redirects to `next`. The path is taken from the redirect URI in the credentials file, so it is configured once, in the console. |
| `POST /auth/logout` | Clears the session, then redirects to `/`. |
| middleware | Reads and verifies the session cookie on every request, so `auth.User` parameters, `@auth.required` and `auth.user(req)` can see who is signed in. |

What a protected route does with no signed-in user:
- A browser navigation (a `GET` whose `Accept` header contains `text/html`) is redirected to the login route, with `next` set to the page it asked for. After signing in, the user lands back on that page.
- Anything else, such as `fetch`, `curl` or another service, gets `401 {"error": "sign-in required"}`. This mirrors Flask-Login's `login_view` and `unauthorized_handler` split, decided per request with no configuration.

Call-style routes and groups get the same protection:

```python
server.get("/admin/stats", auth.protect(stats))       # one handler
admin = server.group("/admin")
admin.use(auth.required)                              # every route in the group
def stats(req: http.Request) -> Result[http.Response]:
    user = auth.user(req).unwrap()                    # `auth.User?`; always Some here
    ...
```

`auth.required` is the same name as the annotation and the middleware, so there is one name to learn.

## 2. Research

The facts below were read from Google's pages on 2026-11-20:
- [OpenID Connect](https://developers.google.com/identity/openid-connect/openid-connect)
- [the OpenID Connect reference](https://developers.google.com/identity/openid-connect/reference)
- [Using OAuth 2.0 for Web Server Applications](https://developers.google.com/identity/protocols/oauth2/web-server)
- [the live discovery document](https://accounts.google.com/.well-known/openid-configuration)

Points marked *check* were not confirmed word for word and must be checked against the live pages before the milestone that relies on them.

### 2.1 The flow Google documents for servers

Google's server flow for sign-in is the OpenID Connect authorization code flow:
1. Create an anti-forgery state token.
2. Send an authentication request to Google.
3. Confirm the state token.
4. Exchange the code for an access token and an ID token.
5. Read the user from the ID token.
6. Authenticate the user.

**Discovery document:** `https://accounts.google.com/.well-known/openid-configuration`. The live values:

| Field | Value |
|---|---|
| `issuer` | `https://accounts.google.com` |
| `authorization_endpoint` | `https://accounts.google.com/o/oauth2/v2/auth` |
| `token_endpoint` | `https://oauth2.googleapis.com/token` |
| `userinfo_endpoint` | `https://openidconnect.googleapis.com/v1/userinfo` |
| `revocation_endpoint` | `https://oauth2.googleapis.com/revoke` |
| `jwks_uri` | `https://www.googleapis.com/oauth2/v3/certs` |
| `id_token_signing_alg_values_supported` | `["RS256"]` |
| `code_challenge_methods_supported` | `["plain", "S256"]` |
| `token_endpoint_auth_methods_supported` | `client_secret_post`, `client_secret_basic` |
| `scopes_supported` | `openid`, `email`, `profile` |
| `authorization_response_iss_parameter_supported` | `true` |

Google's guide uses these endpoints directly. This plan builds them into the Google preset and reads no discovery document at run time; see decision 6.

**Authorization request parameters:**
- `client_id`: required.
- `response_type=code`: required.
- `redirect_uri`: required. It must exactly match a URI registered in the console, including the scheme, case and trailing `/`. Otherwise the request fails with `redirect_uri_mismatch`.
- `scope`: required. It must begin with `openid`, and add `email`, `profile` or both. The default here is `openid email profile`.
- `state`: strongly recommended.
- `nonce`: the OIDC page says required and the reference says optional. It is always sent here.
- Optional:
  - `access_type=online|offline`. `offline` is needed for a refresh token; the default is `online`.
  - `prompt=none|consent|select_account`.
  - `include_granted_scopes=true`.
  - `login_hint`.
  - `hd`: a domain or `*`. It only filters the account picker; Google says not to rely on it for access control.
- PKCE: `code_challenge` and `code_challenge_method=S256`. The discovery document advertises them. The pages read do not describe them for web servers, but the reference's `invalid_grant` error mentions PKCE. *Check* the web-server page's PKCE section. Sending PKCE alongside the client secret is standard OAuth 2.1 practice.

**The callback's query string:**
- On success: `code`, `state`, `scope` and `iss`. Per RFC 9207, `iss` is always `https://accounts.google.com` and should be checked.
- On failure: `error`, one of `access_denied`, `invalid_request`, `unauthorized_client`, `unsupported_response_type` or `invalid_scope`.
- Unknown parameters must be ignored.

**The token exchange:** `POST https://oauth2.googleapis.com/token`, sent as `application/x-www-form-urlencoded`.
- Form fields: `code`, `client_id`, `client_secret`, `redirect_uri`, `grant_type=authorization_code`, plus `code_verifier` with PKCE.
- The JSON reply has `access_token`, `expires_in`, `id_token`, `scope` and `token_type` (always `Bearer`). `refresh_token` is present only with `access_type=offline`, and only on the first consent.
- Errors are JSON `{"error", "error_description"}`:
  - `invalid_grant` (400): the code was used or expired, or the PKCE verifier is wrong.
  - `invalid_client` (401): a wrong secret.
  - Also `invalid_request`, `unauthorized_client`, `unsupported_grant_type` and `invalid_scope`.

**Validating the ID token.** Google lists these steps:
1. Check the signature against the keys at `jwks_uri` (RS256).
2. `iss` is `https://accounts.google.com` or `accounts.google.com`. Both forms must be accepted.
3. `aud` is the client ID.
4. `exp` has not passed.
5. If `hd` was requested, the `hd` claim matches.

The nonce must also equal the one sent, and be used once. Google says a token received directly from the token endpoint, over HTTPS and authenticated with the client secret, can be trusted without checking its signature. It also says any token passed on to other components must be validated. Decision 2 says what this plan does.

**ID token claims:**
- Always present: `iss`, `sub`, `aud`, `iat` and `exp`.
- With the `email` scope: `email` and `email_verified`. `email_verified` is a boolean in the reference, but a string in one of Google's samples, so accept both.
- May be present with the `profile` scope: `name`, `given_name`, `family_name` and `picture`. They are never guaranteed.
- `hd`: present only for Workspace accounts.
- `nonce`: present when one was sent.
- **`sub` is the account's key.** It is at most 255 ASCII characters. Google says not to key accounts on the email, because a user can change it.

**Signing keys:** Google rotates them "infrequently" and says to cache them according to the HTTP cache headers. No other rotation rule is documented. The usual practice is to honour `Cache-Control: max-age`, and refetch once when a token names an unknown `kid`.

**Other endpoints:**
- Userinfo: `GET https://openidconnect.googleapis.com/v1/userinfo` with `Authorization: Bearer <access_token>`. It is not needed, because the ID token carries the same claims.
- Revoke: `POST https://oauth2.googleapis.com/revoke` with the form field `token`. It returns an empty 200.
- Refresh: `POST` to the token endpoint with `grant_type=refresh_token`, `refresh_token`, `client_id` and `client_secret`.

**Security advice from the pages:**
- Prevent login CSRF with `state`, about 30 random characters, kept on the server side.
- Redirect after the callback, so the `code` does not stay in the address bar or the `Referer` header.
- Send access tokens in a header, never in a query string.
- Redirect URIs must use `https`, except for `localhost`, and must match exactly. *Check* the URI validation section for the full rules: no raw IPs except loopback, no fragments, no wildcards.

**The credentials file (`client_secret.json`).** Download it from Cloud console > APIs & Services > Credentials > OAuth client ID > Web application. Google says to keep it out of the source tree; its secret cannot be shown again after the client is created. Its shape, as Go's `google.ConfigFromJSON` and Python's `Flow.from_client_secrets_file` read it, is:

```json
{"web": {
  "client_id": "1234-abc.apps.googleusercontent.com",
  "project_id": "my-project",
  "auth_uri": "https://accounts.google.com/o/oauth2/auth",
  "token_uri": "https://oauth2.googleapis.com/token",
  "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
  "client_secret": "GOCSPX-...",
  "redirect_uris": ["http://localhost:8080/auth/google/callback"],
  "javascript_origins": ["http://localhost:8080"]
}}
```

A desktop client has `"installed"` in place of `"web"`; Go's `ConfigFromJSON` accepts both. *Check* this against a real downloaded file before Milestone 3. The parser must read only `client_id`, `client_secret`, `redirect_uris`, and optionally `token_uri` and `auth_uri`, and ignore everything else.

### 2.2 What other frameworks give the developer

| | Credentials from | Login and callback routes | Session | Protect a route | Signed-out request |
|---|---|---|---|---|---|
| **Flask-Login** (with Authlib or Flask-Dance for Google) | Arguments or `app.config` | Written by the app; it calls `login_user(user)` | Flask's signed cookie session, plus a `remember_token` cookie | `@login_required` | Browser: redirect to `login_view?next=...`. If none is set, 401. A custom `unauthorized_handler` is the documented way to send a 401 to API clients. |
| **Passport** (`passport-google-oauth20`) | Env vars in the example | `passport.authenticate('google')` on two routes the app writes | `express-session`; the app maps Google's profile to its user in a `verify` callback | App-written middleware (`req.isAuthenticated()`) | `failureRedirect` |
| **@fastify/oauth2** | Arguments | `startRedirectPath` is generated; the app writes the callback | The plugin sets `oauth2-redirect-state` and `oauth2-code-verifier` cookies; the user session is up to the app | None built in | Not covered |
| **Go `x/oauth2` + `google`** | `google.ConfigFromJSON(bytes, scopes...)` reads the console's JSON file | The app calls `AuthCodeURL(state)` and `Exchange(code)` | None; the app stores the `Token` | None | Not covered |
| **google-auth-oauthlib** (Python) | `Flow.from_client_secrets_file(path, scopes)` | The app calls `authorization_url()` and `fetch_token()` | None | None | Not covered |
| **Auth.js** (Next.js) | `AUTH_GOOGLE_ID` and `AUTH_GOOGLE_SECRET` env vars | Generated, at `/api/auth/callback/google` | Its own (a JWT cookie by default) | `auth()` in a page, or middleware | Redirect to the sign-in page |

The best parts are taken from each:
- From Go and Google's Python library: read the console's JSON file by path, the way the user asked.
- From Auth.js and Fastify: generate the login and callback routes.
- From Flask and Starlette: a signed cookie session that needs no database.
- From Flask-Login: `@login_required` and the browser-versus-API split.
- From Passport: an `on_login` hook where the app maps Google's account to its own user.
- From Fastify: PKCE and a short-lived, cookie-held `state`.

What no single one of them gives is all of this from two lines (`auth.google(path)` and `install`) with safe defaults. The compiler makes the last step possible: a handler that names `auth.User` is protected because of its type, so nothing about a route has to be written twice.

## 3. What the compiler and library already have

These facts were found by reading the code, and the design relies on them:

- **An HTTPS client.** `http.fetch(method, url, body, headers)` speaks TLS through rustls and the Mozilla roots (`crates/lang_rt/src/client.rs`, `tls.rs`). A task waiting on it parks, so it does not hold a worker. The docstring of `fetch` in `stdlib/http.lang` still says "Only `http://` URLs are supported for now". That is stale; fix it in Milestone 1.
- **`ring` 0.17 is already a direct dependency of `lang_rt`** (`crates/lang_rt/Cargo.toml`), for TLS. It has `rand::SystemRandom`, `digest::SHA256`, `hmac::HMAC_SHA256`, `aead::AES_256_GCM`, and `signature::RsaPublicKeyComponents` with `RSA_PKCS1_2048_8192_SHA256`. That is every primitive this plan needs, so nothing is downloaded and `cargo --offline` keeps working.
- **No crypto, base64 or URL encoding in the standard library.** The `stdlib/` modules are `io`, `os`, `time`, `json`, `log`, `http`, `re` and `postgres`. Only the runtime percent-decodes, for paths and query values (`http.rs` `percent_decode`, `query_value`); nothing encodes.
- **Handler parameters are bound by type** in `check/http.rs` `route_of`:
  - `http.Request` is the request itself.
  - `http.Header` and `http.Cookie` are bound by the parameter's name, and `T?` makes either optional.
  - `{name}` segments of the path, scalar query parameters, and one JSON body are the other bindings.
  - `mount` generates an adapter per route. A `Cookie` binding becomes `__http.cookie_param(__req, "name")?`, and a new binding is one more `Binding` variant and one more generated line.
  - The adapter is nested in `__mount_N(__server, ...)`, so it can reach the server.
- **Annotations.** `check/items.rs` rejects unknown annotations on functions ("unknown annotation `@x` on a function", around line 630). `route_of` takes the first annotation whose name starts with `http.`. Since docs/12 Milestone 1, annotation values can be names, strings, ints, `True`, `False` and lists. `@frozen`-style annotations without arguments already parse.
- **`priv` is private to the file that declares it** (docs/01). A new `stdlib/auth.lang` cannot read `Request.handle` or `Server.routes`. It can use only what `http` makes `pub`.
- **Middleware is `def(Request, Handler) -> Result[Response]`.** `Server.compose` wraps `dispatch` with it, first added outermost, so it runs before the route's adapter and sees the adapter's `Result`. Groups have their own `use`.
- **`Response.headers` is a `dict[str, str]`,** written out as `name: value` lines by `Server.serve` (`stdlib/http.lang` around line 697). **A response cannot carry two `Set-Cookie` headers.** The callback must set the session cookie and clear the flow cookie in the same response.
- **`Request.cookie(name)` exists**, but there is no way to set a cookie apart from writing the `Set-Cookie` header by hand.
- **Test requests are limited.** `http.test_request(method, path, body="")` has no headers or cookies, and `req.query` returns `None` for it, so a query string in its path is not split off. Testing a protected route, or the callback, needs both fixed.
- **Shared state.** `Mutex` and `Atomic` exist, and a value given to `mount` must be `Shareable`. The provider object holds the cached signing keys, so it needs a `Mutex` around them.
- **Environment and files:** `os.getenv(name) -> str?` and `io.read_file(path) -> Result[str, IoError]`. JSON goes through `json.decode[T]` with `@derive(Json)`.
- `openssl` is installed here, so Milestone 4's test fixture key can be generated once and committed.

## 4. Design

### 4.1 Public API

**The `auth` module, `stdlib/auth.lang`:**

```python
pub def google(
    credentials: str = "",              # path to client_secret.json; "" reads GOOGLE_CLIENT_SECRETS (a path),
                                        # else GOOGLE_CLIENT_ID + GOOGLE_CLIENT_SECRET
    secret: str? = None,                # session signing key, at least 32 bytes; None reads SESSION_SECRET
    redirect_uri: str = "",             # "" uses the only one in the file (an error if there are several)
    scopes: list[str] = ["openid", "email", "profile"],
    allowed_domains: list[str] = [],    # Workspace domains (the `hd` claim); [] allows any account
    allowed_emails: list[str] = [],     # exact addresses; [] allows any
    require_verified_email: bool = True,
    session_days: int = 7,              # how long a sign-in lasts
    login_path: str = "/auth/google/login",
    logout_path: str = "/auth/logout",
    after_logout: str = "/",
    on_login: (def(User, Tokens) -> Result[User, HttpError])? = None,   # map, record or refuse a user
) -> Result[Provider, AuthError]
```

**`Provider`:**
- `provider.install(server)` and `provider.install(group)` add the routes and the session middleware.
- `provider.login_url(next: str = "/") -> str` gives the URL of the login route, for links.
- `provider.test_cookie(user: User) -> str` is a valid `Cookie` header value, for tests (section 4.6).
- `Provider` is `Shareable`.

**`User`,** `@derive(Json) @frozen`, built from the ID token:
- `id: str`: the `sub` claim.
- `email: str`, with `email_verified: bool`.
- `name: str`, `given_name: str`, `family_name: str` and `picture: str`. Each is `""` when absent.
- `domain: str`: the `hd` claim, or `""`.
- `provider: str`: `"google"`.

**`Tokens`:** `access_token`, `refresh_token: str?`, `expires_at: float`, `scope: str` and `id_token: str`. They are only handed to `on_login`, never stored by the module (decision 3).

**For routes:**
- `@auth.required` marks a route.
- `required(req, next)` is a `Middleware`, for `server.use`, `group.use`, and what the annotation generates.
- `protect(handler) -> http.Handler` wraps one call-style handler.
- `user(req) -> User?` is the signed-in user, for call-style handlers and middleware.
- `logout_response(location="/") -> http.Response` clears the session, for apps that write their own logout route.

**Handler parameter bindings,** added to `check/http.rs`:
- `user: auth.User`: protected. With no user, the request is refused as in section 1, and the handler is not called.
- `user: auth.User?`: never refused; `None` when signed out.

**In `http` (Milestone 1):**
- `Response.set_cookie(name, value, max_age=-1, path="/", http_only=True, secure=False, same_site="Lax")` and `Response.delete_cookie(name, path="/")`. Each adds a cookie, kept in a `priv cookies: list[str]`, and any number can be set on one response. `serve` writes each as its own `Set-Cookie` line.
- `http.url_encode(text) -> str`, `http.url_decode(text) -> str?`, and `http.form_encode(fields: list[(str, str)]) -> str` for `application/x-www-form-urlencoded` bodies and query strings.
- `http.post_form(url, fields, headers={})`.
- `http.test_request(method, path, body="", headers: dict[str, str] = {})`. It now splits a query string off `path` and answers `query()`, `header()` and `cookie()` from these values.
- A per-request store for middleware: `req.set_local(name: str, value: str)` and `req.local(name) -> str?`. This is Express's `res.locals` and Starlette's `request.state`. It is a `priv locals: dict[str, str]? = None`, allocated on first `set_local`, so requests that do not use it pay nothing. The hot path is guarded by the HTTP benchmark gate.

**A new module, `crypto` (`stdlib/crypto.lang`),** backed by `ring` in a new `crates/lang_rt/src/crypto.rs`:
- `random_bytes(n) -> bytes` and `token(n_bytes=32) -> str` (URL-safe base64).
- `sha256(data: bytes) -> bytes` and `hmac_sha256(key: bytes, data: bytes) -> bytes`.
- `equal(a: bytes, b: bytes) -> bool`, which runs in constant time.
- `base64_encode`, `base64_decode`, `base64url_encode` and `base64url_decode`.
- `verify_rs256(modulus: bytes, exponent: bytes, message: bytes, signature: bytes) -> bool`.
- AES-256-GCM `seal` and `open`. These are only for decision 3's "keep tokens" option, and may be left out if that is left out.

All of it is public. These are the primitives every web app eventually needs: CSRF tokens, signed links, webhook signature checks.

### 4.2 The session cookie

- **Name:** `__Host-session` when the redirect URI is `https`. The `__Host-` prefix forces `Secure`, `Path=/` and no `Domain`, so a subdomain cannot plant it. On `http://localhost` the name is `session`, without `Secure`.
- **Value:** `base64url(payload) + "." + base64url(HMAC-SHA256(key, "v1." + payload))`. The payload is the JSON of `{"u": User, "iat": issued, "exp": expires}`.
- **The key** is derived: `hmac_sha256(secret, "auth session v1")`, so the raw secret is never the MAC key, and the flow cookie uses `"auth flow v1"`, so one cookie's MAC can never pass as the other's. The language's name is not part of these strings.
- **Attributes:** `HttpOnly`, `SameSite=Lax` and `Max-Age = session_days`.
- **Checks:** the middleware recomputes the MAC, compares it in constant time, rejects an expired `exp`, decodes `User`, and stores the payload with `req.set_local("auth.user", json)`.
- **Size:** a few hundred bytes, well under browsers' 4 KB.
- Rotating `SESSION_SECRET` signs everyone out. `previous_secrets=[...]` is left out (section 4.7).

### 4.3 The flow, step by step

**Login: `GET login_path?next=/path`.**
1. Validate `next`:
   - It must start with `/` and not `//` or `/\`, and contain no control characters.
   - Otherwise use `/`. This prevents open redirects (Flask-Login's warning).
2. Generate:
   - `state = crypto.token()`.
   - `nonce = crypto.token()`.
   - `verifier = crypto.token(48)`.
   - `challenge = base64url(sha256(verifier))`.
3. Set the flow cookie (`__Host-oauth` or `oauth`). It is signed like the session cookie and holds `{state, nonce, verifier, next, exp: now + 600}`, with `Max-Age=600`, `HttpOnly` and `SameSite=Lax`. Lax is required: the callback is a top-level cross-site `GET` from Google, and `Strict` would drop the cookie.
4. Redirect with a 302 to the authorization endpoint. The parameters are `client_id`, `redirect_uri`, `response_type=code`, `scope` (space-joined), `state`, `nonce`, `code_challenge`, `code_challenge_method=S256` and `include_granted_scopes=true`. `hd` is added when `allowed_domains` has exactly one entry, as a hint only. Every value goes through `http.url_encode`.

**Callback: `GET redirect_uri's path`.**
1. Read and verify the flow cookie. If it is missing, expired or tampered with, return a 400 page saying "sign-in expired, try again", with a link to the login route.
2. If `error` is present (for example `access_denied`), clear the flow cookie and return a 401 page that names the error and links to the login route. Nothing is logged at error level.
3. `state` must equal the cookie's value, compared in constant time. If `iss` is present, it must be `https://accounts.google.com`.
4. `POST` the token endpoint with `post_form`: `grant_type=authorization_code`, `code`, `redirect_uri`, `client_id`, `client_secret` and `code_verifier`.
   - A transport failure is a 502.
   - An `invalid_grant` is a 400 "sign-in expired, try again". It happens when the code is reused, for example on a page reload.
   - Other errors are a 502 with Google's `error` logged. The secret is never logged.
5. Validate the ID token (section 4.4).
6. Apply the policy, failing with a 403 page that names the account:
   - `require_verified_email` must be met.
   - The `hd` claim must be in `allowed_domains`.
   - The email must be in `allowed_emails`.
7. Call `on_login(user, tokens)`, if given. `Err(e)` is that error's status and message, so the app can refuse. `Ok(user)` may replace fields, for example with the app's own display name; `id` must stay `sub`.
8. Respond with a 302 to `next`. The response sets the session cookie and deletes the flow cookie, which is why `set_cookie` and multiple cookies come first. The redirect also takes `code` out of the address bar.

**Logout: `POST logout_path`.**
- If the `Origin` header is present, it must match the host (a `GET` is not accepted).
- It deletes the session cookie and redirects with a 303 to `after_logout`.
- Signing out of Google itself is not the app's business and is not done. Revoking tokens is not needed, because none are kept.

**Protection.**
- `required(req, next)`: if `user(req)` is `None`, refuse; otherwise return `next(req)`.
- To refuse:
  - For a `GET` or `HEAD` whose `Accept` contains `text/html`, redirect with a 302 to `login_url(req.path + query)`.
  - Otherwise return `401 {"error": "sign-in required"}` with `WWW-Authenticate: Cookie realm="google"`. The header is informational; there is no standard cookie scheme.
- With `@auth.required` and an `auth.User` parameter, the generated adapter does the same check before binding anything else. A signed-out request therefore gets a 401 or a redirect, never a 400 about its body.

### 4.4 Validating the ID token

Google allows skipping the signature check for a token that came straight from the token endpoint. This plan **checks it anyway** (decision 2), because:
- It is cheap: one RSA verification per sign-in, not per request.
- It guards against a misconfigured `token_uri`.
- The same code path will serve the generic OpenID Connect providers of section 4.7, where the rule may not hold.

The steps:
1. Split the token into three base64url parts. The header must have `"alg": "RS256"`, otherwise reject. Read `kid`.
2. Find the key in the cached JWKS. On a miss, refetch once; the fetch is protected by the provider's `Mutex`, so concurrent sign-ins fetch once. Reject an unknown `kid` after the refetch. Cache the keys for the response's `Cache-Control: max-age`, at least 1 hour and at most 24 hours; with no header, 1 hour.
3. `crypto.verify_rs256(n, e, header + "." + payload, signature)`.
4. Check the claims:
   - `iss` is in `{"https://accounts.google.com", "accounts.google.com"}`.
   - `aud` equals `client_id`. If `aud` is a list, it must contain `client_id`, and then `azp` must equal `client_id`.
   - `exp > now - 60` and `iat < now + 60` (60 seconds of clock skew).
   - `nonce` equals the flow cookie's nonce.
5. Decode `email_verified` from either a boolean or the string `"true"`/`"false"`.

Every failure is a 401 "could not verify the sign-in" page. The specific reason is logged at warning level, with the token itself never logged.

### 4.5 Compiler changes (`check/http.rs`, `check/items.rs`)

The compiler finds the `auth` types the way `find_http_types` finds `http`'s: `auth.User` and the `auth` module, and only when the program imports `auth`.

**`route_of`:**
- A parameter of type `auth.User` becomes `Binding::User { optional: false }`, and `auth.User?` becomes `{ optional: true }`.
- `@auth.required` on a route sets `Route.protected = true`. So does a required `auth.User` parameter.
- `@auth.required` on a function with no `@http.*` annotation is an error. Its help line says: "`@auth.required` protects a route; for a call-style handler use `auth.protect(handler)`".

**`check/items.rs`:** accept `auth.required` as a function annotation when `auth` is imported, without arguments. With arguments, report a `BAD_ROUTE`-style error: "takes no arguments; restrict users with `allowed_domains=` or check `user` in the handler".

**`mount`:** a protected route's adapter starts with a guard line, before any other binding, and a `User` binding reads the stored user:

```text
def __route_3(__req: Request) -> Result[Response]:
    match __auth.guard(__req):                    # Some(refusal) when signed out
        case Some(__refusal):
            return Ok(__refusal)
        case None:
            pass
    __a0 = __auth.user_param(__req)?              # Binding::User, required
    __result = __value.me(__a0)
    return __http.reply_json(__result, 200)
```

The optional form is `__auth.user(__req)`. `guard` and `user_param` are `pub` functions of `auth`, and their docstrings say they are for generated code, as `http.reply_json`'s does.

**The OpenAPI document (docs/12):**
- When `auth` is installed and `server.docs()` is called, add a security scheme, `"googleSession": {"type": "apiKey", "in": "cookie", "name": "<the cookie>"}`.
- Each protected operation gets `security: [{"googleSession": []}]` and a `401` response, without the user writing `security=`.
- The docs page's "Try it out" sends the browser's cookie on its own, because it is same-origin.

**Errors to add to docs/errors.md, with codes:**
- `@auth.required` without a route.
- `auth.User` in a WebSocket handler. It is supported later; for now it is an error with a help line saying to read the cookie from the upgrade request.
- Two `auth.User` parameters.

### 4.6 Testing, for the language's users

```python
@test
def test_notes_need_sign_in():
    app = make_server()
    assert app.handle(http.test_request("GET", "/notes")).status == 401
    browser = http.test_request("GET", "/notes", headers={"Accept": "text/html"})
    assert app.handle(browser).status == 302

@test
def test_notes_signed_in():
    google = test_provider()
    cookie = google.test_cookie(auth.User(id="1", email="a@example.com", email_verified=True))
    response = app.handle(http.test_request("GET", "/notes", headers={"Cookie": cookie}))
    assert response.status == 200
```

- `auth.google_for_tests(secret="...")` builds a provider without a credentials file, whose login route only redirects to a fixed URL. It is used together with `test_cookie`, which signs with the provider's key.
- No test of a user's program ever contacts Google.

### 4.7 Out of scope

These are left out, and section 7 lists them for later:
- Generic OpenID Connect providers: Microsoft, Okta, Auth0 and Keycloak through `auth.oidc(discovery_url, ...)`. The `Provider` struct holds the endpoints and the issuer, so this is a constructor, not a redesign.
- GitHub sign-in, which is OAuth without OIDC.
- Server-side sessions in Postgres.
- Keeping and refreshing access tokens for calling Google APIs (decision 3).
- Roles and permissions.
- `previous_secrets` for rotating keys.
- The Google Identity Services "One Tap" button, whose ID token arrives by `POST` and needs CSRF handling with a double-submit cookie.
- `auth.User` in WebSocket handlers.

## 5. Owner decisions

**Confirmed by the owner on 2026-11-20: all seven defaults below stand.** The alternatives are kept for the record.

1. **The module is called `auth`, and the Google preset is `auth.google(...)`.** This leaves room for `auth.oidc` and `auth.github`. The alternative is a `google` module, which reads well but does not grow.
2. **ID token signatures are verified (JWKS and RS256)** even though Google allows skipping it for tokens straight from the token endpoint. This costs the `crypto.verify_rs256` work and a JWKS cache, and makes Milestone 4 larger. Skipping it would remove most of Milestone 4.
3. **The session is a signed cookie holding only the user.** It is stateless, needs no database, and works across restarts and several instances that share the `SESSION_SECRET`. Google's tokens are not kept; an app that wants them gets them in `on_login` and stores them itself. The alternative is an encrypted cookie (AES-GCM) that keeps the refresh token; it adds a `keep_tokens=True` option and the AEAD functions.
4. **Without `secret=` or `SESSION_SECRET`:**
   - The default makes a random key for the process and logs a warning that sign-ins end when the program restarts. This suits `localhost` development.
   - When the redirect URI is `https` (production), it refuses to start instead.
   - Always refusing to start is the alternative.
5. **The browser-or-API refusal is automatic,** based on `Accept: text/html` and the method. Alternatives are always a 401, or an `on_refused=` option.
6. **Endpoints are built in, not discovered.** The Google preset carries the endpoint URLs from section 2.1, so starting needs no network call and tests are simpler. `auth.oidc` (later) would read a discovery document.
7. **`crypto` is a public module.** The alternative is to keep these functions private to `auth`, and publish them later.

## 6. Milestones

Each milestone follows docs/11's rules: the gates (`make lint`, `make test`, `make site-check`, and the benchmark checks when the hot path is touched), the docs updated in the same commit, and one commit and push per milestone. Each ends with a **Done** note added here.

### Milestone 1: the pieces `http` lacks (about 1.5 days)

- `Response.set_cookie` and `delete_cookie`, with several `Set-Cookie` lines written by `serve`. Validate cookie names and values (no `;`, `,` or control characters; a 500 with a clear message otherwise).
- `http.url_encode`, `url_decode`, `form_encode` and `post_form`. Expose the runtime's `percent_decode` and add an encoder in `crates/lang_rt/src/http.rs`. Encode the RFC 3986 unreserved set as-is and everything else as `%XX`.
- `test_request(..., headers={})`, with the query string split off the path. `query()`, `header()` and `cookie()` must work on test requests.
- `req.set_local` and `req.local`, lazily allocated.
- Fix `fetch`'s stale "Only http:// URLs" docstring.
- **Gate:** run `bench/http` before and after. `set_cookie` and `locals` must not change the plaintext and JSON numbers beyond noise.
- **Tests:**
  - `tests/run/` programs for multiple cookies and URL round-trips, including multi-byte UTF-8 and `+`.
  - The cookie attributes, checked against a real `curl -i` in the HTTP server tests.
- **Docs:** docs/05 (HTTP), and the site's requests-and-responses section.

**Done** (D100). Everything listed, with these differences:
- Several values of one header are newline-joined in the one `headers` entry, and `serve` writes a line each. There is a public `response.add_header(name, value)` for any header. `response.cookie(name)` reads a cookie back, for tests. The client keeps repeated response headers the same way.
- `set_cookie` percent-encodes only what a cookie cannot hold (and `%`), and `req.cookie` decodes `%XX`. An invalid name, path or `same_site` panics, which is a 500. `__Host-`, `__Secure-` and `SameSite=None` cookies are always `Secure`.
- The test-request parts and the locals share one optional field that replaced the cached body text, so `Request` did not grow. Two separate fields had cost 16 bytes per request in `bench/http/counts.py`. `req.text()` now reads the connection's buffer on each call.
- The test is `tests/run/http_cookies_and_encoding.wb`, with a real server and client in one program, which checks the `Set-Cookie` lines on the wire. No `curl` test was added.
- The gates passed: tests, stress (300 runs), clippy, the formatter, the site's examples, and the HTTP counts (unchanged). `bench/run.py --check` failed on different unrelated benchmarks on two runs (`sort` and `spawn`, then `json_orders`' memory) and passed on the third, which is noise.

### Milestone 2: `crypto` (about 1.5 days)

- `crates/lang_rt/src/crypto.rs` on `ring`, with the functions of section 4.1. `random_bytes` uses `SystemRandom` and panics if the OS has no randomness, which never happens in practice.
- `stdlib/crypto.lang`, registered in `lang_sema/src/stdlib.rs`.
- **Tests:**
  - Known-answer tests: SHA-256 of `""` and `"abc"`, HMAC-SHA256 from RFC 4231 test cases 1 and 2, and base64 from RFC 4648 section 10.
  - `verify_rs256` against a fixture signed with `openssl dgst -sha256 -sign`, a correct and a tampered message.
  - `equal` returns `False` for different lengths.
  - A `tests/ui` golden for a wrong argument type.
- **Docs:** a `crypto` section in docs/05 and on the site's standard library page.

### Milestone 3: `auth` without signatures (about 2.5 days)

- `stdlib/auth.lang`:
  - The `google(...)` constructor reads the credentials file (`web` or `installed`), with clear `AuthError` messages: the file is missing, the JSON is not the console's, there is no `client_secret`, there are several redirect URIs and no `redirect_uri=`, or a redirect URI is not `http://localhost` or `https`.
  - `Provider`, `User`, `Tokens`, `install`, the login, callback and logout routes, the session and flow cookies, and the policy checks.
  - `required`, `protect`, `user`, `logout_response`, `test_cookie` and `google_for_tests`.
- In this milestone the ID token's claims are checked but its signature is not. This is Google's documented allowance, and the next milestone adds the check.
- **Tests:** a test program runs a **fake Google** on `127.0.0.1:0`: its authorization route redirects straight back with a code, and its token route returns a JSON reply with an unsigned ID token. The provider is pointed at it through `google_for_tests(endpoints=...)`. The tests cover:
  - The whole flow: login, then the redirect to the callback, the cookie, and a protected page. The test follows the redirects with the client.
  - A wrong `state`, a missing flow cookie, `error=access_denied`, `invalid_grant`, a wrong `aud`, an expired `exp`, a wrong `nonce`, a disallowed domain, an unverified email, and an `on_login` refusal.
  - `next` cannot leave the site (`//evil.com`, `/\evil.com`, `https://evil.com`).
  - A tampered or expired session cookie.
  - Logout with a foreign `Origin`.

### Milestone 4: signatures and the key cache (about 1.5 days)

- Fetch the JWKS, cache it under a `Mutex`, honour `max-age`, refetch once on an unknown `kid`, and use `verify_rs256`.
- The fake Google signs its ID tokens with a committed **test-only** RSA key: `tests/fixtures/oidc_test_key.pem`, generated with `openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048`. It serves the matching JWKS, and its comment says it is a fixture.
  - Signing in the test needs RSA signing, which `crypto` does not offer. The fake Google is therefore a small Rust test helper (`crates/lang_cli/tests/oidc_fake.rs`) using `ring::signature::RsaKeyPair`. The alternative of adding `sign_rs256` to `crypto` was rejected: it is not needed by users yet.
- **Tests:**
  - A good signature.
  - A tampered payload.
  - `alg: none` and `alg: HS256`, both rejected.
  - An unknown `kid`, then a refetch and success after the fake rotates keys.
  - Two concurrent sign-ins cause one JWKS fetch.

### Milestone 5: the compiler (about 2 days)

- Everything in section 4.5: the `auth.User` and `auth.User?` bindings, `@auth.required`, the guard line, the new errors (with `tests/ui` goldens and docs/errors.md entries), and the OpenAPI security scheme and 401 for protected routes.
- The LSP: hovering over `@auth.required` shows its docstring. The editor grammars need nothing, since they already highlight annotations.
- **Tests:**
  - `tests/run` programs for a mounted app with each form of protection.
  - Signed-out API and browser requests to each.
  - The OpenAPI test in `crates/lang_cli/tests/openapi.rs` extended with a protected route.

### Milestone 6: documentation and a real check (about 1 day)

- Docs:
  - A "Signing in with Google" section in docs/05, and a page section on the site: setting up the Cloud console client, the redirect URI, `SESSION_SECRET`, protecting routes, testing, and deploying behind a proxy, where the redirect URI must be the public `https` URL.
  - The site's examples are checked by `site/check_examples.py`; network-dependent ones are `data-check="compile"`.
- `examples/google_login/` (or the repository's examples location): a small app with a home page, a protected notes page and `/me`.
- **A manual check with a real Google client.** It needs the owner's Cloud project, and cannot run in CI. The owner, or the agent with the owner's file, runs the example on `http://localhost:8080`, signs in, and records the result in the Done note. An agent never puts the owner's `client_secret.json` in the repository or in a commit; it is ignored by a `.gitignore` entry for `client_secret*.json` added in this milestone.
- Release notes.

### What will be built, in total

About 10 days of work: two new modules (`crypto`, `auth`), five additions to `http`, two new parameter bindings, one annotation, and documentation.

## 7. Risks and open questions

- **Redirect URIs behind proxies.** Behind a TLS-terminating proxy, the app sees `http`, but the redirect URI must be the public `https://...`. The callback's path is taken from the registered URI, not from the request, so this works. The docs must say to register the public URL. Whether to make the session cookie `Secure` follows the redirect URI's scheme, for the same reason.
- **Clock skew.** 60 seconds of leeway on `exp` and `iat`. A server with a badly wrong clock fails every sign-in; the warning log says "token expired" with both times.
- **Google changes.** The endpoints are built in (decision 6). Google has kept them stable for years, and the discovery document is there if they move. A future `auth.oidc` reads it.
- **Session revocation.** A stateless cookie cannot be revoked before it expires, except by rotating the secret. This is documented, and server-side sessions are a later option.
- **`SameSite=Lax` and the callback.** Lax cookies are sent on top-level cross-site `GET` navigations, which is how Google redirects back. If a browser ever stops sending them, the flow cookie goes missing and the callback says "sign-in expired". Fastify's plugin relies on the same behaviour.
- **Cookie size.** Long names and picture URLs fit easily. `on_login` can add nothing to the cookie, so it cannot grow beyond the `User` fields.
- **Owner's credentials.** The real-Google check in Milestone 6 needs them, and nothing else does.
- **Not verified word for word**, to check before the milestone that relies on each:
  - The PKCE section of Google's web-server page (Milestone 3).
  - The full `client_secret.json` shape against a downloaded file (Milestone 3).
  - The redirect URI validation rules (Milestone 3).
  - The JWKS response's actual `Cache-Control` (Milestone 4).

## 8. Sources

- Google: [OpenID Connect](https://developers.google.com/identity/openid-connect/openid-connect), [OpenID Connect reference](https://developers.google.com/identity/openid-connect/reference), [OAuth 2.0 for Web Server Applications](https://developers.google.com/identity/protocols/oauth2/web-server), [discovery document](https://accounts.google.com/.well-known/openid-configuration), [Verify the Google ID token](https://developers.google.com/identity/gsi/web/guides/verify-google-id-token).
- Frameworks: [Flask-Login](https://flask-login.readthedocs.io/en/latest/), [@fastify/oauth2](https://github.com/fastify/fastify-oauth2), [passport-google-oauth20](https://github.com/jaredhanson/passport-google-oauth2), [Go golang.org/x/oauth2](https://pkg.go.dev/golang.org/x/oauth2) and [its google package](https://pkg.go.dev/golang.org/x/oauth2/google), [Auth.js Google provider](https://authjs.dev/getting-started/providers/google), [google-auth-oauthlib Flow](https://google-auth-oauthlib.readthedocs.io/en/latest/reference/google_auth_oauthlib.flow.html).
- Authlib's Flask client page and Starlette's middleware page could not be fetched (404 and DNS failure), so neither is relied on.
