# /// script
# requires-python = ">=3.11"
# dependencies = ["openai==2.8.0", "anthropic==0.72.0"]
# ///
"""Model calls in OpenAI and Anthropic shape, with the official SDKs, through the runtime's model proxy.
Usage: uv run providers.py openai|anthropic   -> prints {"plain", "stream", "tool"?} or {"error"}
openai: Gemini's OpenAI-compatible API (free key in GEMINI_API_KEY): plain, streamed, a tool call.
anthropic: plain and streamed. There is no free key: the fixture holds the SDK's real requests,
recorded with an invalid key, and hand-written answers in Anthropic's format."""
import json, os, sys

PROXY = 'http://127.0.0.1:9223'
ASK = [{'role': 'user', 'content': 'Reply with exactly: the lamp is cheapest'}]
TOOL = {'type': 'function', 'function': {'name': 'add_to_cart', 'description': 'Add a product to the cart',
        'parameters': {'type': 'object', 'properties': {'id': {'type': 'string'}}, 'required': ['id']}}}
out = {}
try:
    if sys.argv[1] == 'openai':
        from openai import OpenAI
        c = OpenAI(base_url=f'{PROXY}/v1beta/openai/', api_key=os.environ.get('GEMINI_API_KEY', 'invalid'), max_retries=0)
        m = os.environ.get('GEMINI_MODEL', 'gemini-3.5-flash-lite')
        out['plain'] = c.chat.completions.create(model=m, messages=ASK).choices[0].message.content
        out['stream'] = ''.join(ch.choices[0].delta.content or '' for ch in c.chat.completions.create(model=m, messages=ASK, stream=True) if ch.choices)
        r = c.chat.completions.create(model=m, tools=[TOOL], tool_choice='required', messages=[{'role': 'user', 'content': 'Add the lamp to the cart.'}])
        out['tool'] = [[t.function.name, t.function.arguments] for t in r.choices[0].message.tool_calls or []]
    else:
        from anthropic import Anthropic
        c = Anthropic(base_url=PROXY, api_key='invalid', max_retries=0)
        out['plain'] = c.messages.create(model='claude-sonnet-5-5', max_tokens=50, messages=ASK).content[0].text
        with c.messages.stream(model='claude-sonnet-5-5', max_tokens=50, messages=ASK) as s:
            out['stream'] = ''.join(s.text_stream)
except Exception as e:
    out['error'] = f'{type(e).__name__}: {str(e)[:200]}'
print(json.dumps(out))
