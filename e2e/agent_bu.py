# /// script
# requires-python = ">=3.11"
# dependencies = ["browser-use==1.0.4"]
# ///
"""A real Browser Use 1.0 agent on the replayed shop. The model is Gemini (free tier), via GEMINI_API_KEY.
llm must be explicit: without it, browser-use 1.0 uses (and bills) its cloud model."""
import asyncio, json, os
from browser_use import Agent, Browser

TASK = ('Open http://127.0.0.1:8001/ and add the cheapest product to the cart. '
        'Then report the cart count and the order number shown on the page.')


async def main():
    agent = Agent(TASK, llm=f"google:{os.environ['GEMINI_MODEL']}", browser=Browser(cdp_url='http://127.0.0.1:9222'))
    result = await agent.run(max_steps=15, timeout=300)
    print(json.dumps({'answer': result.final_result(), 'errors': result.errors()}))


asyncio.run(main())
