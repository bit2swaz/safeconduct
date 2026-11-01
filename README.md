# safeconduct

Open infrastructure to test, contain and prove what a web agent did, and on whose behalf.

> A *safe-conduct* is a document that lets a traveler pass safely through foreign territory.
> safeconduct does the same for AI agents on the web.

**Status:** early. Nothing works yet.

## Plan

| Stage | Product | What it does |
|---|---|---|
| 1 | replay browser | Deterministic record/replay for browser agents. Seeded faults, rules checked after every step, shrinking to the smallest failing run. |
| 2 | guard | Open policy engine between an agent and the browser (CDP and MCP layers). |
| 3 | receipts | Verifiable session receipts: zkTLS attestations, archive, deterministic re-render, hash-chained action log. |
| 4 | gate | Open, self-hostable gate for sites: agent signatures, delegation, access policy, HTTP 402. |

## License

Apache-2.0
