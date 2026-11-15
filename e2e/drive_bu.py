# /// script
# requires-python = ">=3.11"
# dependencies = ["browser-use==1.0.4"]
# ///
"""Drive the shop through Browser Use 1.0, attached with cdp_url like any agent would.
Usage: uv run drive_bu.py [--probe]   -> prints {"text", "probe"}"""
import asyncio, json, os, sys, time
from pathlib import Path
from browser_use import Browser

steps = json.loads((Path(__file__).parent / 'steps.json').read_text())


async def main():
    b = Browser(cdp_url='http://127.0.0.1:9222')
    os.environ.update(await b.start())  # tells the helpers which browser-harness daemon to use
    from browser_harness import helpers as h

    def until(expr):
        for _ in range(100):
            if h.js(expr):
                return
            time.sleep(0.1)
        raise TimeoutError(expr)

    h.goto_url(steps['url'])
    until(steps['ready'])
    h.js(steps['addCheapest'])
    until(steps['added'])
    text = h.js(steps['text'])
    # In exact mode the probe is off the recording, so the runtime ends the run right after it.
    probe = h.js(steps['probe']) if '--probe' in sys.argv else None
    print(json.dumps({'text': text, 'probe': probe}), flush=True)
    try:
        await asyncio.wait_for(b.stop(), 5)
    except Exception:
        pass


asyncio.run(main())
