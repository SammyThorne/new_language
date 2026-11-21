# Sign in with Google: an example

A small notes app whose pages and API need a Google account: a home page, a protected notes page, a JSON API under `/api/notes`, `/me`, and its API documentation at `/docs`. `app.vi` is the app, `main.vi` serves it with a real Google client, and `app_test.vi` tests it without one.

## Running it

1. In the [Google Cloud console](https://console.cloud.google.com/apis/credentials), choose or create a project, then **Create credentials → OAuth client ID**. If asked, set up the consent screen first: an "External" app in testing, with your own address as a test user, is enough.
2. Choose the type **Web application**, and add the **authorized redirect URI** `http://localhost:8080/auth/google/callback`.
3. Download the client's JSON file into this folder as `client_secret.json`. It holds the client secret: `.gitignore` keeps it out of the repository, and it must never be committed.
4. Give the app a key for its session cookies, and run it:

   ```bash
   export SESSION_SECRET="$(head -c 32 /dev/urandom | base64)"
   viper run main.vi
   ```

   Or copy `.env.example` to `.env` and fill it in: `main.vi` reads it with `config.load_env()`, and variables already set in the environment win. `.gitignore` keeps `.env` out of the repository.

5. Open <http://localhost:8080> and sign in. Without `SESSION_SECRET` the app still runs on `localhost`, with a key made for the run and a warning, so sign-ins end when it stops.

Instead of the file, the app also takes `GOOGLE_CLIENT_SECRETS` (the file's path), or `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET`.

## Testing it

```bash
viper test
```

The tests use `auth.google_for_tests()` and sign requests in with `test_cookie`; nothing contacts Google.

## Deploying it

The redirect URI must be the address users see, such as `https://notes.example.com/auth/google/callback`, added to the client in the console. Behind a proxy that ends TLS, give it explicitly: `auth.google("client_secret.json", redirect_uri="https://notes.example.com/auth/google/callback")`. With an `https` redirect URI the cookies are `Secure` and named `__Host-session`, and `SESSION_SECRET` is required, at least 32 bytes. See "Signing in with Google" in [docs/05-standard-library.md](../../docs/05-standard-library.md).
