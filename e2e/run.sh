#!/usr/bin/env bash
# safeconduct end to end. Every phase so far, on the full stack:
#   B1a    record the live shop, kill it, replay it through Playwright, Stagehand and Browser Use.
#          A request missing from the recording raises off-recording and ends the run (exit 3).
#   B1a    real LLM agents (Stagehand v4, Browser Use 1.0) do a task on the replayed shop.  [--live]
#   B1b-1  exact mode: replay a real agent run (site + model + the agent's own site requests)
#          twice, with the site down and an invalid API key. Both must repeat it exactly.
#   B1b-2  fuzz mode: seeded faults on the replayed shop. The same seed gives the same faults and
#          the same page; off-recording gets a fault and the run goes on. With --live, real agents
#          run into a cookie-wall popup (seed 5).
#   B1b-3  rules: the TS and Python clients start the runtime and check rules after every step.
#          A clean run has no violations; paying twice and leaving the shop's origin are caught,
#          at the right steps, by both clients alike. With --live, real agents buy and pay with
#          the rules on (Stagehand via the TS client, Browser Use via the Python one).
#   B1b-4  fingerprints: the same behavior gives the same fingerprint (record, replays, both
#          clients). shrink: a naive buyer pays twice under 6 seeded faults; shrink finds the one
#          fault that causes it (the dropped payment).
#
# Usage: e2e/run.sh                 agent runs come from e2e/fixtures: 0 model calls, no key
#        e2e/run.sh --live          fresh live agent runs; close every phase with this
#        e2e/run.sh --live --save-fixtures   also store the new agent runs as fixtures
# --live needs GEMINI_API_KEY in ../.env (free: https://aistudio.google.com/apikey).
# Needs: cargo, node, uv, python3, and Playwright's Chromium (npx playwright install chromium).
# Each run keeps its logs in e2e/out/<time>/ (e2e/out/latest is the newest).
set -euo pipefail
cd "$(dirname "$0")"
LIVE=0 SAVE=0
for a in "$@"; do
  case $a in --live) LIVE=1 ;; --save-fixtures) SAVE=1 ;; *) echo "unknown flag: $a"; exit 2 ;; esac
done
export ANONYMIZED_TELEMETRY=false BH_TELEMETRY=0 BROWSER_USE_NO_UPDATE_CHECK=1 BH_UPDATE_CHECK=0
cargo build -q --manifest-path ../Cargo.toml
bin=../target/debug/safeconduct
export SAFECONDUCT_BIN=$PWD/$bin PYTHONPATH=$PWD/../clients/py # the clients start the runtime themselves
LLM=http://127.0.0.1:9223 # the runtime's model proxy (CDP port + 1)
[ -d node_modules ] || npm ci --silent
out=out/$(date +%Y%m%d-%H%M%S)
mkdir -p $out && ln -sfn "$(basename $out)" out/latest
ls -1d out/2* | head -n -20 | xargs rm -rf # keep the last 20 runs
pids=()
trap 'kill ${pids[@]+"${pids[@]}"} 2>/dev/null || true' EXIT

site_up() { python3 ../sites/shop/server.py 8001 & site=$!; pids+=($site); }
site_down() {
  kill $site; wait $site 2>/dev/null || true
  ! curl -s -o /dev/null --max-time 1 http://127.0.0.1:8001/ || { echo "site still up"; exit 1; }
}
start() { # start <mode> <recording> <log> [flags]: run the runtime and wait for its ready event
  $bin "$1" "$2" "${@:4}" > "$3" 2>&1 & rt=$!; pids+=($rt)
  for _ in $(seq 100); do grep -q '"ready"' "$3" && return; sleep 0.1; done
  echo "runtime did not start:"; cat "$3"; exit 1
}
stop() { # stop the runtime; its exit code goes to $code (3 = exact mode left the recording)
  kill -INT "$rt" 2>/dev/null || true
  code=0; wait "$rt" || code=$?
}
drive() { # drive <client> [--probe]
  case $1 in
    playwright|stagehand) timeout 120 node drive.mjs "$@" ;;
    browser-use) shift; timeout 180 uv run -q drive_bu.py "$@" ;;
  esac
}
agent() { # agent <name>: one real agent task, prints {"answer": ...}
  case $1 in
    stagehand) timeout 180 node agent_stagehand.mjs ;;
    # Model calls and the agent's own site requests both go through the runtime.
    browser-use) GOOGLE_GEMINI_BASE_URL=$LLM HTTP_PROXY=$LLM http_proxy=$LLM timeout 180 uv run -q agent_bu.py ;;
  esac 2>> $out/agent.err | tail -1
}
fail=0

echo "== B1a record (live site)"
site_up
start record $out/shop.jsonl $out/record.log
drive playwright | tail -1 > $out/record.json
stop
site_down
echo "   $(wc -l < $out/shop.jsonl) responses recorded; site stopped"

for client in playwright stagehand browser-use; do
  echo "== B1a replay via $client"
  start replay $out/shop.jsonl $out/replay-$client.log
  drive $client --probe | tail -1 > $out/replay-$client.json
  stop
  python3 - $out/record.json $out/replay-$client.json $out/replay-$client.log $code <<'PY' || fail=1
import json, sys
rec, rep = (json.load(open(p)) for p in sys.argv[1:3])
off = [json.loads(l) for l in open(sys.argv[3]) if '"off-recording"' in l]
checks = {
    'same page as record run': rep['text'] == rec['text'],
    'clock starts at T0 (2023-11-14)': 'today: 2023-11-14' in rep['text'],
    'cart updated from recording': 'cart: 1 order #' in rep['text'],
    'unrecorded request blocked': rep['probe'] == 'blocked',
    'off-recording event raised': any(e['request']['url'].endswith('/api/never-recorded') for e in off),
    'off-recording ended the run (exit 3)': sys.argv[4] == '3',
}
for name, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {name}")
if not checks['same page as record run']:
    print('   record:', json.dumps(rec['text'])); print('   replay:', json.dumps(rep['text']))
sys.exit(0 if all(checks.values()) else 1)
PY
done

if [ -f ../.env ]; then set -a; . ../.env; set +a; fi
export GEMINI_MODEL=${GEMINI_MODEL:-gemini-3.5-flash-lite}
if [ $LIVE = 1 ] && [ -z "${GEMINI_API_KEY:-}" ]; then echo "--live needs GEMINI_API_KEY in .env"; exit 2; fi

if [ $LIVE = 1 ]; then
  for name in stagehand browser-use; do
    # A live model varies: it can get the task wrong, or take an action the recording does not
    # have (off-recording). So this check gets one retry; a runtime bug fails both tries.
    # The exact-mode and fuzz-determinism checks never retry.
    for try in 1 2; do
      echo "== B1a real agent: $name ($GEMINI_MODEL, live model) on the replayed shop$([ $try = 2 ] && echo ', retry')"
      start replay $out/shop.jsonl $out/agent-$name-$try.log --live-model
      agent $name > $out/agent-$name-$try.json || true
      stop
      res=0
      python3 - $out/record.json $out/agent-$name-$try.json $out/agent-$name-$try.log <<'PY' || res=$?
import json, re, sys
order = re.search(r'order #(\d+)', json.load(open(sys.argv[1]))['text']).group(1)
try:
    answer = json.load(open(sys.argv[2]))
except ValueError:
    answer = {'answer': None}
events = [json.loads(l) for l in open(sys.argv[3]) if l.startswith('{"')]
cart = [e for e in events if e.get('url', e.get('request', {}).get('url', '')).endswith('/api/cart')]
off = any(e['event'] == 'off-recording' for e in events)
checks = {
    'agent added to cart, served from recording': any(e['event'] == 'served' for e in cart),
    'no request off the recording': not off,
    f'answer has the recorded order #{order}': order in str(answer.get('answer')),
}
for name, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {name}")
print('   answer:', str(answer.get('answer'))[:200])
sys.exit(0 if all(checks.values()) else 2)  # 2 = retry once
PY
      [ $res = 2 ] && [ $try = 1 ] && { echo "   (the live model missed the task or left the recording; retrying once)"; continue; }
      [ $res = 0 ] || fail=1
      break
    done
  done
else
  echo "== B1a real agents: skipped (they need a live model: --live)"
fi

for name in stagehand browser-use; do
  rec=fixtures/exact-$name.jsonl answer0=fixtures/exact-$name.answer.json
  if [ $LIVE = 1 ]; then
    echo "== B1b-1 exact mode: $name (fresh live recording)"
    rec=$out/exact-$name.jsonl answer0=$out/exact-$name-0.json
    for try in 1 2; do # a live model that fails the task gives nothing worth replaying: retry once
      site_up
      start record $rec $out/exact-$name-0.log
      agent $name > $answer0 || true
      stop
      site_down
      grep -q '"answer": *[^n]' $answer0 && break
      [ $try = 1 ] && echo "   (the live model missed the task while recording; retrying once)"
    done
  else
    echo "== B1b-1 exact mode: $name (fixture)"
    [ -f $rec ] || { echo "   FAIL no fixture: run e2e/run.sh --live --save-fixtures"; fail=1; continue; }
  fi
  for i in 1 2; do
    start replay $rec $out/exact-$name-$i.log
    GEMINI_API_KEY=invalid-key-so-nothing-reaches-the-model agent $name > $out/exact-$name-$i.json || true
    stop; echo $code > $out/exact-$name-$i.code
  done
  ok=0
  python3 - $name $out $rec $answer0 <<'PY' || { ok=1; fail=1; }
import json, os, re, sys
name, out, rec_path, answer0 = sys.argv[1:5]
def answer(path):
    try:
        return json.load(open(path)).get('answer')
    except ValueError:
        return None
def events(i):
    return [json.loads(l) for l in open(f'{out}/exact-{name}-{i}.log') if l.startswith('{"')]
rec = open(rec_path).read()
recorded = answer(answer0)
llm_recorded = sum(1 for l in rec.splitlines() if json.loads(l).get('llm'))
served = {i: [(e.get('method'), e['url'], e.get('index')) for e in events(i) if e['event'] == 'served'] for i in (1, 2)}
drift = [e for i in (1, 2) for e in events(i) if e['event'] == 'llm-drift']
# Browser Use puts a random temp dir (/tmp/browser-use-xxxx) in its prompt: agent-side noise, not ours.
noise = re.compile(r'browser-use-[a-z0-9_]*')
real_drift = [e for e in drift if noise.sub('', e['diff']['recorded']) != noise.sub('', e['diff']['now'])]
codes = [open(f'{out}/exact-{name}-{i}.code').read().strip() for i in (1, 2)]
key = os.environ.get('GEMINI_API_KEY', '')
checks = {
    f'recorded a real run ({llm_recorded} model calls)': llm_recorded > 0 and recorded is not None,
    'replay 1 gives the recorded answer': answer(f'{out}/exact-{name}-1.json') == recorded,
    'replay 2 gives the recorded answer': answer(f'{out}/exact-{name}-2.json') == recorded,
    'both replays serve the same requests in the same order': served[1] == served[2] and len(served[1]) > 0,
    'every recorded model call was replayed': sum(1 for s in served[1] if s[2] is not None) == llm_recorded,
    'nothing off the recording': codes == ['0', '0'],
    f'no prompt drift beyond agent temp dirs ({len(drift)} drift events)': not real_drift,
    'API key never stored': not key or key not in rec,
}
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
print('   answer:', str(recorded)[:160])
for e in real_drift[:3]:
    print('   drift:', json.dumps(e['diff'])[:300])
sys.exit(0 if all(checks.values()) else 1)
PY
  if [ $SAVE = 1 ] && [ $LIVE = 1 ] && [ $ok = 0 ]; then
    mkdir -p fixtures && cp $rec fixtures/exact-$name.jsonl && cp $answer0 fixtures/exact-$name.answer.json
    echo "   saved as fixture"
  fi
done
echo "== B1b-2 fuzz mode: same seed, same faults (Playwright, rate 0.5)"
for run in 3a 3b 4; do
  start replay $out/shop.jsonl $out/fuzz-$run.log --fuzz ${run%[ab]} --fault-rate 0.5
  drive playwright --fuzz $([ $run = 4 ] && echo --probe) | tail -1 > $out/fuzz-$run.json
  stop; echo $code > $out/fuzz-$run.code
done
python3 - $out <<'PY' || fail=1
import json, sys
out = sys.argv[1]
def faults(run):
    return [(e['kind'], e['request']) for e in map(json.loads, open(f'{out}/fuzz-{run}.log')) if e.get('event') == 'fault']
def page(run):
    return json.load(open(f'{out}/fuzz-{run}.json'))
probe_faults = [f for f in faults('4') if f[1].endswith('/api/never-recorded')]
off = [e for e in map(json.loads, open(f'{out}/fuzz-4.log')) if e.get('event') == 'off-recording']
checks = {
    f"seed 3 injects faults ({', '.join(k for k, _ in faults('3a'))})": len(faults('3a')) > 0,
    'seed 3 twice: the same faults in the same order': faults('3a') == faults('3b'),
    'seed 3 twice: the same page': page('3a')['text'] == page('3b')['text'],
    'seed 4 gives different faults': faults('4')[:len(faults('3a'))] != faults('3a'),
    f"off-recording gets a fault ({page('4')['probe']}), not the end of the run": len(probe_faults) == 1 and off and not off[0]['run_ends'] and open(f'{out}/fuzz-4.code').read().strip() == '0',
}
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
sys.exit(0 if all(checks.values()) else 1)
PY

if [ $LIVE = 1 ]; then
  for name in stagehand browser-use; do
    echo "== B1b-2 fuzz mode: real agent $name ($GEMINI_MODEL, live model) vs a cookie-wall popup (seed 5)"
    start replay $out/shop.jsonl $out/fuzz-agent-$name.log --fuzz 5 --fault-rate 0.3
    agent $name > $out/fuzz-agent-$name.json || true
    stop; echo $code > $out/fuzz-agent-$name.code
    python3 - $out $name <<'PY' || fail=1
import json, sys
out, name = sys.argv[1:3]
events = [json.loads(l) for l in open(f'{out}/fuzz-agent-{name}.log') if l.startswith('{"')]
faults = [e for e in events if e['event'] == 'fault']
try:
    result = json.load(open(f'{out}/fuzz-agent-{name}.json'))
except ValueError:
    result = {}
answer = result.get('answer')
cart = [e for e in events if e['event'] == 'served' and e.get('url', '').endswith('/api/cart')]
checks = {
    f"the agent's page got the cookie wall ({len(faults)} faults in its run)": any(f['kind'] == 'popup' and f['request'].endswith(':8001/') for f in faults),
    'the run went to the end (runtime exit 0)': open(f'{out}/fuzz-agent-{name}.code').read().strip() == '0',
    'the agent reported an outcome (an answer or an error)': answer is not None or 'error' in result,
}
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
# What fuzzing is for: how the agent copes. A finding, not a test failure.
print(f"   finding: {'added to cart through the faults' if cart else 'did NOT add to cart'}; answer: {str(answer)[:120]}"
      + (f"; error: {result['error'][:120]}" if result.get('error') else ''))
sys.exit(0 if all(checks.values()) else 1)
PY
  done
fi
echo "== B1b-3 rules: TS and Python clients, Playwright on the replayed shop"
for c in ts py; do for mode in clean bad; do
  case $c in
    ts) timeout 120 node rules.mjs $out/shop.jsonl $mode ;;
    py) timeout 180 uv run -q rules.py $out/shop.jsonl $mode ;;
  esac 2>> $out/rules.err | tail -1 > $out/rules-$c-$mode.json || true
done; done
python3 - $out <<'PY' || fail=1
import json, sys
out = sys.argv[1]
def result(c, mode):
    try:
        return json.load(open(f'{out}/rules-{c}-{mode}.json'))
    except ValueError:
        return {'code': None, 'steps': 0, 'violations': [{'rule': 'no result', 'step': 0, 'message': ''}]}
def found(r):  # [(rule, step)], rule names alike in both languages (noDoublePayment = no_double_payment)
    return [(v['rule'].replace('_', '').lower(), v['step']) for v in r['violations']]
checks = {}
for c in ('ts', 'py'):
    clean, bad = result(c, 'clean'), result(c, 'bad')
    f = dict(found(bad))
    checks[f"{c}: clean run, no violations ({clean['steps']} steps)"] = clean['code'] == 0 and clean['violations'] == [] and clean['steps'] >= 3
    checks[f"{c}: paying twice, then leaving the origin: both caught ({found(bad)})"] = (
        sorted(f) == ['allowedorigins', 'nodoublepayment'] and len(found(bad)) == 2 and f['nodoublepayment'] < f['allowedorigins'])
    checks[f"{c}: leaving the origin was off the recording too (exit {bad['code']})"] = bad['code'] == 3
checks['both clients report the same violations at the same steps'] = (
    found(result('ts', 'bad')) == found(result('py', 'bad'))
    and [v['message'] for v in result('ts', 'bad')['violations']] == [v['message'] for v in result('py', 'bad')['violations']])
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
sys.exit(0 if all(checks.values()) else 1)
PY

echo "== B1b-4 fingerprints and shrink"
$bin shrink -- node shrink_test.mjs fixtures/shop.jsonl 299 0.5 2>> $out/shrink.err > $out/shrink.log || true
$bin shrink -- true > $out/shrink-pass.log 2>&1 && echo 0 > $out/shrink-pass.code || echo $? > $out/shrink-pass.code
python3 - $out <<'PY' || fail=1
import json, sys
out = sys.argv[1]
def end(log):  # the runtime's end event: fingerprint and step count
    return next((json.loads(l) for l in open(log) if l.startswith('{"') and '"end"' in l), {})
def fp(f):
    try:
        return json.load(open(f)).get('fingerprint')
    except ValueError:
        return None
shrunk = [json.loads(l) for l in open(f'{out}/shrink.log') if '"shrunk"' in l]
s = shrunk[0] if shrunk else {}
kinds = [(f['kind'], f['request']) for f in s.get('faults', [])]
exact = {n: [end(f'{out}/exact-{n}-{i}.log').get('fingerprint') for i in (1, 2)] for n in ('stagehand', 'browser-use')}
checks = {
    f"record, TS client and Python client runs share one fingerprint ({end(f'{out}/record.log').get('fingerprint')})":
        end(f'{out}/record.log').get('fingerprint') == fp(f'{out}/rules-ts-clean.json') == fp(f'{out}/rules-py-clean.json') is not None,
    'exact replays: the same fingerprint twice, for each agent': all(a == b and a for a, b in exact.values()),
    'fuzz: the same seed gives the same fingerprint': end(f'{out}/fuzz-3a.log').get('fingerprint') == end(f'{out}/fuzz-3b.log').get('fingerprint'),
    'a different behavior gives a different fingerprint': fp(f'{out}/rules-ts-clean.json') != fp(f'{out}/rules-ts-bad.json'),
    f"shrink: {s.get('of')} faults down to {kinds} in {s.get('trials')} trials":
        len(kinds) == 1 and kinds[0][0] in ('drop', 'error') and kinds[0][1].endswith('/api/pay'),
    'shrink: the result fails again on its own': s.get('reproduces') is True,
    'shrink: a passing test gives nothing to shrink (exit 1)': open(f'{out}/shrink-pass.code').read().strip() == '1',
}
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
print(f"   reproduce: {s.get('reproduce', '')}")
sys.exit(0 if all(checks.values()) else 1)
PY

if [ $LIVE = 1 ]; then
  for name in stagehand browser-use; do
    echo "== B1b-3 rules: real agent $name ($GEMINI_MODEL, live model) buys and pays, rules on"
    (export SC_RULES=$out/shop.jsonl; agent $name) > $out/rules-agent-$name.json || true
    python3 - $out $name <<'PY' || fail=1
import json, sys
out, name = sys.argv[1:3]
try:
    r = json.load(open(f'{out}/rules-agent-{name}.json'))
except ValueError:
    r = {}
reqs = r.get('requests', [])
pays = [(q['url'], q.get('post')) for q in reqs if q['method'] == 'POST' and '/api/pay' in q['url']]
# The rules again, over the whole request log: the client must agree.
expect = {'nodoublepayment'} if len(pays) > len(set(pays)) else set()
expect |= {'allowedorigins'} if any(not q['url'].startswith('http://127.0.0.1:8001/') for q in reqs) else set()
got = {v['rule'].replace('_', '').lower() for v in r.get('violations', [])}
checks = {
    f"rules ran after every step ({r.get('steps', 0)} steps, {len(reqs)} requests)": r.get('steps', 0) >= 1 and len(reqs) > 0,
    f"violations match the request log ({sorted(got) or 'none'})": got == expect,
    'the agent reported an outcome (an answer or an error)': r.get('answer') is not None or 'error' in r or r.get('errors'),
}
for k, ok in checks.items():
    print(f"   {'ok  ' if ok else 'FAIL'} {k}")
print(f"   finding: {len(pays)} payment(s); violations: {[v['message'][:80] for v in r.get('violations', [])] or 'none'};"
      f" runtime exit {r.get('code')}; answer: {str(r.get('answer'))[:100]}")
sys.exit(0 if all(checks.values()) else 1)
PY
  done
fi
if grep -q 'PerDay.*FreeTier' $out/agent.err 2>/dev/null; then
  echo "   NOTE the free model quota for the day ran out during this run: live results after that mean nothing"
fi
[ $fail = 0 ] && echo "e2e: PASS" || { echo "e2e: FAIL"; exit 1; }
