# safeconduct

open infrastructure to test, contain and prove what a web agent did, and on whose behalf

> A *safe-conduct* is a document that lets a traveler pass safely through foreign territory.
> safeconduct does the same for AI agents on the web.

**Status:** early. Stage 1 has started: the replay browser v0 records and replays a site for unmodified agents.

## Replay browser

`safeconduct` launches headless Chromium and exposes one CDP endpoint. Connect any agent to it as you would to Chrome.

```sh
npx playwright install chromium            # the browser it drives
cargo run -- record run.jsonl              # agents use http://127.0.0.1:9222; site and model calls are saved
cargo run -- replay run.jsonl              # exact mode: the same run again, from run.jsonl only
cargo run -- replay run.jsonl --live-model # the recorded site, a live model
```

**Exact mode** repeats a whole agent run: the site, the agent's model calls and its own HTTP requests come from the recording, with the site down and no API key.
- Model calls from inside the browser (Stagehand v4) are caught automatically.
- An agent that calls its model from its own process points its base URL at the model proxy on port 9223 (`GOOGLE_GEMINI_BASE_URL`, `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`). Set `HTTP_PROXY` to the same port so its own site requests are recorded too.
- API keys are never stored.

Every page and worker gets a seeded RNG (`Math.random`, `crypto`) and a clock that starts at 2023-11-14. Each agent command waits until the page is settled, and site responses replay in recorded order.
A request that is not in the recording prints an `off-recording` event and ends the run (exit code 3). A model prompt that differs from the recording prints `llm-drift`.

Tested with Playwright (`connectOverCDP`), Stagehand v4 (`localBrowser.connect`) and Browser Use 1.0 (`Browser(cdp_url=...)`).
`e2e/run.sh` checks everything end to end from saved fixtures (no model calls). `e2e/run.sh --live` uses real agents and a real model.

## Plan

| Stage | Product | What it does |
|---|---|---|
| 1 | replay browser | Deterministic record/replay for browser agents. Seeded faults, rules checked after every step, shrinking to the smallest failing run. |
| 2 | guard | Open policy engine between an agent and the browser (CDP and MCP layers). |
| 3 | receipts | Verifiable session receipts: zkTLS attestations, archive, deterministic re-render, hash-chained action log. |
| 4 | gate | Open, self-hostable gate for sites: agent signatures, delegation, access policy, HTTP 402. |

## License

Apache-2.0
