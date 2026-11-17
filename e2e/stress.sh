#!/usr/bin/env bash
# safeconduct stress and race pass. Free: fixtures only, no model calls.
#   race    $PAR runtimes at the same time, on different ports, under full CPU load. Each pair
#           (Playwright, the same fuzz seed, the Stagehand and Browser Use exact fixtures) must
#           give the same fingerprint, faults and answer in every runtime.
#   stress  e2e/run.sh $RUNS times under full CPU load. Prints how often each check failed.
# Usage: e2e/stress.sh [RUNS=5] [PAR=4]
# Logs: e2e/out/stress-<time>/
set -euo pipefail
cd "$(dirname "$0")"
RUNS=${1:-5} PAR=${2:-4}
cargo build -q --manifest-path ../Cargo.toml
bin=../target/debug/safeconduct
export GEMINI_MODEL=${GEMINI_MODEL:-gemini-3.5-flash-lite} GEMINI_API_KEY=invalid-key-so-nothing-reaches-the-model
export ANONYMIZED_TELEMETRY=false BH_TELEMETRY=0 BROWSER_USE_NO_UPDATE_CHECK=1 BH_UPDATE_CHECK=0
out=out/stress-$(date +%Y%m%d-%H%M%S); mkdir -p $out
pids=()
trap 'kill ${pids[@]+"${pids[@]}"} 2>/dev/null || true' EXIT
for _ in $(seq $(nproc)); do (while :; do :; done) & pids+=($!); done # full CPU load for the whole pass
echo "load: $(nproc) busy loops"

# race <name> <rt flags> -- <client command, run with SC_CDP and the model proxy of its runtime>
race() {
  local name=$1 flags=$2; shift 3
  local rts=() clients=()
  for k in $(seq $PAR); do
    local port=$((9300 + 2 * k)) llm="http://127.0.0.1:$((9301 + 2 * k))"
    $bin replay $flags --port $port > $out/$name-$k.log 2>&1 & rts+=($!)
    ( for _ in $(seq 100); do grep -q '"ready"' $out/$name-$k.log && break; sleep 0.1; done
      SC_CDP=http://127.0.0.1:$port GOOGLE_GEMINI_BASE_URL=$llm HTTP_PROXY=$llm http_proxy=$llm timeout 300 "$@" 2>/dev/null | tail -1 > $out/$name-$k.json || true
      kill -INT ${rts[$((k - 1))]} 2>/dev/null || true ) & clients+=($!)
  done
  for p in "${clients[@]}" "${rts[@]}"; do wait $p || true; done
}
echo "== race: $PAR runtimes at once"
race playwright "fixtures/shop.jsonl" -- node drive.mjs playwright
race fuzz "fixtures/shop.jsonl --fuzz 299 --fault-rate 0.5" -- node drive.mjs playwright --fuzz
race stagehand "fixtures/exact-stagehand.jsonl" -- node agent_stagehand.mjs
race browser-use "fixtures/exact-browser-use.jsonl" -- uv run -q agent_bu.py
set +e
python3 - $out $PAR <<'PY'
import json, sys
out, par = sys.argv[1], int(sys.argv[2])
def log(name, k):
    return [json.loads(l) for l in open(f'{out}/{name}-{k}.log') if l.startswith('{"')]
def field(name, k, key):
    try:
        return json.load(open(f'{out}/{name}-{k}.json')).get(key)
    except ValueError:
        return None
def same(xs):
    return len(xs) == par and all(x == xs[0] for x in xs) and xs[0] not in (None, [], '')
ok = True
for name, what in [('playwright', 'text'), ('fuzz', 'text'), ('stagehand', 'answer'), ('browser-use', 'answer')]:
    fps = [next((e['fingerprint'] for e in log(name, k) if e['event'] == 'end'), None) for k in range(1, par + 1)]
    checks = {f'{name}: one fingerprint in {par} runtimes ({len(set(fps))} seen)': same(fps),
              f'{name}: the same {what} in every runtime': same([field(name, k, what) for k in range(1, par + 1)])}
    if name == 'fuzz':
        checks['fuzz: the same faults in every runtime'] = same([sorted(e['id'] for e in log(name, k) if e['event'] == 'fault') for k in range(1, par + 1)])
    for k, good in checks.items():
        print(f"   {'ok  ' if good else 'FAIL'} {k}")
        ok &= good
sys.exit(0 if ok else 1)
PY
[ $? = 0 ] && race_ok=0 || race_ok=1
set -e

echo "== stress: e2e/run.sh $RUNS times under load"
failed=0
for i in $(seq $RUNS); do
  s=$(date +%s)
  ./run.sh > $out/run-$i.txt 2>&1 && r=PASS || { r=FAIL; failed=$((failed + 1)); }
  echo "   run $i: $r ($(( $(date +%s) - s ))s, logs e2e/$(readlink out/latest | sed 's#^#out/#'))"
done
echo "== failed checks (count of $RUNS runs)"
fails=$(cat $out/run-*.txt | grep -h "FAIL" | grep -v "e2e: FAIL" | sed 's/([^)]*)//g' | sort | uniq -c | sort -rn || true)
echo "${fails:-   none}"
[ $failed = 0 ] && [ $race_ok = 0 ] && { echo "stress: PASS"; exit 0; }
echo "stress: FAIL ($failed of $RUNS runs failed$([ $race_ok = 0 ] || echo ', race failed'))"; exit 1
