# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright==1.58.0"]
# ///
"""The Python client's rules on a replayed shop, driven by Playwright.
Usage: PYTHONPATH=../clients/py uv run rules.py <recording> clean|bad   -> prints {"code", "steps", "violations", "fingerprint"}
clean: buy the cheapest product once. bad: pay twice, then leave the shop's origin."""
import json, sys
from dataclasses import asdict
from pathlib import Path
from playwright.sync_api import sync_playwright
from safeconduct import allowed_origins, no_double_payment, replay

steps = json.loads((Path(__file__).parent / 'steps.json').read_text())
file, mode = sys.argv[1:3]

run = replay(file, rules=[allowed_origins(['http://127.0.0.1:8001']), no_double_payment('/api/pay')])
with sync_playwright() as p:
    browser = p.chromium.connect_over_cdp(run.endpoint)
    page = browser.contexts[0].new_page()
    until = lambda expr: page.wait_for_function(expr, timeout=10000)
    page.goto(steps['url'])
    until(steps['ready'])
    page.evaluate(steps['addCheapest'])
    until(steps['added'])
    page.evaluate(steps['pay'])
    until(steps['paid'])
    if mode == 'bad':
        page.evaluate(steps['pay'])
        page.wait_for_timeout(500)
        try:
            page.goto('http://127.0.0.1:8002/')  # off the recording too: the run ends
        except Exception:
            pass
    try:
        browser.close()
    except Exception:
        pass
code = run.close()
print(json.dumps({'code': code, 'steps': run.steps, 'violations': [asdict(v) for v in run.violations], 'fingerprint': run.fingerprint}))
