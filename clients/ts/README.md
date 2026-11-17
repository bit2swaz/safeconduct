# safeconduct client

The thin client for [safeconduct](https://github.com/bit2swaz/safeconduct), a replay browser for AI agents. It starts the `safeconduct` runtime, gives your agent a CDP address, and checks your rules after each step of the agent.

You need the runtime on your `PATH` (or set `SAFECONDUCT_BIN`) and a Chromium from Playwright:

```sh
cargo install safeconduct
npx playwright install chromium
```

See the [main README](https://github.com/bit2swaz/safeconduct#rules) for the API and examples.
