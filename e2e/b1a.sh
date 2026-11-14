#!/usr/bin/env bash
# B1a end to end: record the live shop once, kill the shop, then replay it through
# Playwright, Stagehand and Browser Use. Every replay must render exactly what the record run saw,
# and a request missing from the recording must raise an off-recording event.
# With GEMINI_API_KEY in ../.env (free: https://aistudio.google.com/apikey), real LLM agents
# (Stagehand v4, Browser Use 1.0) then each do a task on the replayed shop.
# Needs: cargo, node, uv, python3, and Playwright's Chromium (npx playwright install chromium).
set -euo pipefail
cd "$(dirname "$0")"
export ANONYMIZED_TELEMETRY=false BH_TELEMETRY=0 BROWSER_USE_NO_UPDATE_CHECK=1 BH_UPDATE_CHECK=0
cargo build -q --manifest-path ../Cargo.toml
bin=../target/debug/safeconduct
[ -d node_modules ] || npm ci --silent
rm -rf out && mkdir out
pids=()
trap 'kill ${pids[@]+"${pids[@]}"} 2>/dev/null || true' EXIT

start() { # start <mode> <log>: run the runtime and wait for its ready event
  $bin "$1" out/shop.jsonl > "$2" 2>&1 & rt=$!; pids+=($rt)
  for _ in $(seq 100); do grep -q '"ready"' "$2" && return; sleep 0.1; done
  echo "runtime did not start:"; cat "$2"; exit 1
}
stop() { kill -INT "$rt"; wait "$rt" || true; }
drive() { # drive <client> [--probe]
  case $1 in
    playwright|stagehand) timeout 120 node drive.mjs "$@" ;;
    browser-use) shift; timeout 180 uv run -q drive_bu.py "$@" ;;
  esac
}

echo "== record (live site)"
python3 ../sites/shop/server.py 8001 & site=$!; pids+=($site)
start record out/record.log
drive playwright | tail -1 > out/record.json
stop
kill $site
echo "   $(wc -l < out/shop.jsonl) responses recorded; site stopped"
curl -s -o /dev/null --max-time 1 http://127.0.0.1:8001/ && { echo "site still up"; exit 1; } || true

fail=0
for client in playwright stagehand browser-use; do
  echo "== replay via $client"
  start replay out/replay-$client.log
  drive $client --probe | tail -1 > out/replay-$client.json
  stop
  python3 - out/record.json out/replay-$client.json out/replay-$client.log <<'PY' || fail=1
import json, sys
rec, rep = (json.load(open(p)) for p in sys.argv[1:3])
off = [json.loads(l) for l in open(sys.argv[3]) if '"off-recording"' in l]
checks = {
    'same page as record run': rep['text'] == rec['text'],
    'clock starts at T0 (2023-11-14)': 'today: 2023-11-14' in rep['text'],
    'cart updated from recording': 'cart: 1 order #' in rep['text'],
    'unrecorded request blocked': rep['probe'] == 'blocked',
    'off-recording event raised': any(e['url'].endswith('/api/never-recorded') for e in off),
}
for name, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {name}")
if not checks['same page as record run']:
    print('   record:', json.dumps(rec['text'])); print('   replay:', json.dumps(rep['text']))
sys.exit(0 if all(checks.values()) else 1)
PY
done
if [ -f ../.env ]; then set -a; . ../.env; set +a; fi
if [ -n "${GEMINI_API_KEY:-}" ]; then
  export GEMINI_MODEL=${GEMINI_MODEL:-gemini-3.5-flash-lite}
  for agent in stagehand browser-use; do
    echo "== real agent: $agent ($GEMINI_MODEL) on the replay"
    start replay out/agent-$agent.log
    case $agent in
      stagehand) timeout 120 node agent_stagehand.mjs ;;
      browser-use) timeout 180 uv run -q agent_bu.py ;;
    esac 2> out/agent-$agent.err | tail -1 > out/agent-$agent.json || true
    stop
    python3 - out/record.json out/agent-$agent.json out/agent-$agent.log out/shop.jsonl <<'PY' || fail=1
import json, re, sys
rec = json.load(open(sys.argv[1]))
order = re.search(r'order #(\d+)', rec['text']).group(1)
try:
    answer = json.load(open(sys.argv[2]))
except ValueError:
    answer = {'answer': None}
events = [json.loads(l) for l in open(sys.argv[3]) if l.startswith('{"')]
cart = [e for e in events if e.get('url', '').endswith('/api/cart')]
model_hosts = ('googleapis.com', 'generativelanguage')
checks = {
    'agent added to cart, served from recording': any(e['event'] == 'served' for e in cart),
    'no cart request off the recording': not any(e['event'] == 'off-recording' for e in cart),
    f'answer has the recorded order #{order}': order in str(answer.get('answer')),
    'model traffic never touched the recording': not any(h in open(sys.argv[4]).read() + json.dumps(events) for h in model_hosts),
}
for name, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {name}")
print('   answer:', str(answer.get('answer'))[:200])
sys.exit(0 if all(checks.values()) else 1)
PY
  done
else
  echo "== real agents skipped: no GEMINI_API_KEY in .env"
fi
[ $fail = 0 ] && echo "B1a e2e: PASS" || { echo "B1a e2e: FAIL"; exit 1; }
