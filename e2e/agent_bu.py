# /// script
# requires-python = ">=3.11"
# dependencies = ["browser-use==1.0.4"]
# ///
"""A real Browser Use 1.0 agent on the replayed shop. The model is Gemini (free tier), via GEMINI_API_KEY.
llm must be explicit: without it, browser-use 1.0 uses (and bills) its cloud model.
SC_RULES=<recording>: start the runtime here through the Python client (PYTHONPATH=../clients/py),
with the built-in rules, and also pay. Then it prints {answer, errors, code, steps, violations, requests}."""
import asyncio, json, os
from dataclasses import asdict
from browser_use import Agent, Browser

TASK = ('Open http://127.0.0.1:8001/ and add the cheapest product to the cart. '
        'Then report the cart count and the order number shown on the page.')


PAY_TASK = ('Open http://127.0.0.1:8001/ and add the cheapest product to the cart, then pay for the order. '
            'Then report the cart count and the order number shown on the page.')


async def main():
    run = None
    if os.environ.get('SC_RULES'):
        from safeconduct import allowed_origins, no_double_payment, replay
        rules = [allowed_origins(['http://127.0.0.1:8001']), no_double_payment('/api/pay')]
        run = replay(os.environ['SC_RULES'], live_model=True, rules=rules)
    browser = Browser(cdp_url=run.endpoint if run else 'http://127.0.0.1:9222')
    agent = Agent(PAY_TASK if run else TASK, llm=f"google:{os.environ['GEMINI_MODEL']}", browser=browser)
    result = await agent.run(max_steps=15, timeout=300)
    out = {'answer': result.final_result(), 'errors': result.errors()}
    if run:
        await browser.stop()
        out |= {'code': run.close(), 'steps': run.steps, 'violations': [asdict(v) for v in run.violations],
                'requests': [e for e in run.events if e['event'] == 'request']}
    print(json.dumps(out))


asyncio.run(main())
