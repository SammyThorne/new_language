# Sign in with GitHub

A small app that signs users in with GitHub, which is OAuth 2 without an ID token: after the
code exchange the app reads the account from GitHub's API.

1. In GitHub, open **Settings > Developer settings > OAuth Apps > New OAuth App**.
2. Set the **Authorization callback URL** to `http://localhost:8080/auth/github/callback` (later
   your public `https://.../auth/github/callback`).
3. Generate a client secret. Copy `.env.example` to `.env` and fill in the three values.
4. `viper run main.vi`, then open http://localhost:8080.

`auth.github()` asks for the `read:user` and `user:email` scopes. The user's email is their
primary address when GitHub has verified it; without one, sign-in is refused (pass
`require_verified_email=False` to allow it, with `User.email == ""`). `User.id` is
`github:<number>` and does not change when the user renames their account.

`viper test` runs the tests, which contact nothing.
