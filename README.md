# safeconduct

Open infrastructure to test, contain and prove what a web agent did, and on whose behalf.

> A *safe-conduct* is a document that lets a traveler pass safely through foreign territory.
> safeconduct does the same for AI agents on the web.

**Status:** early. Stage 1 has started: the replay browser v0 records and replays a site for unmodified agents.

## Replay browser v0

`safeconduct` launches headless Chromium and exposes one CDP endpoint. Connect any agent to it as you would to Chrome.

```sh
npx playwright install chromium            # the browser it drives
cargo run -- record shop.jsonl             # agents use http://127.0.0.1:9222; the network is saved
cargo run -- replay shop.jsonl             # the same site, served from shop.jsonl only
```

In both modes, every page and worker gets a seeded RNG (`Math.random`, `crypto`) and a clock that starts at 2023-11-14.
In replay, a request that is not in the recording fails and prints an `off-recording` event.

Tested with Playwright (`connectOverCDP`), Stagehand v4 (`localBrowser.connect`) and Browser Use 1.0 (`Browser(cdp_url=...)`).
Run `e2e/b1a.sh` to check all three end to end.

## Plan

| Stage | Product | What it does |
|---|---|---|
| 1 | replay browser | Deterministic record/replay for browser agents. Seeded faults, rules checked after every step, shrinking to the smallest failing run. |
| 2 | guard | Open policy engine between an agent and the browser (CDP and MCP layers). |
| 3 | receipts | Verifiable session receipts: zkTLS attestations, archive, deterministic re-render, hash-chained action log. |
| 4 | gate | Open, self-hostable gate for sites: agent signatures, delegation, access policy, HTTP 402. |

## License

Apache-2.0
