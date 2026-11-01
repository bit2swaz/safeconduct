#!/usr/bin/env bash
# cross-machine check: run the all-fixes config in many containers, compare hashes with the host
# run: ./cross.sh [N=30]
set -u
cd "$(dirname "$0")"
N=${1:-30}
PW=mcr.microsoft.com/playwright:v1.64.1
INSTALL='npx playwright install --with-deps chromium >/dev/null 2>&1'

cases=(
  # name|docker run flags, image last|setup before the spike
  "host||"
  "ubuntu 24.04 (pw noble)|$PW-noble|"
  "ubuntu 22.04 (pw jammy)|$PW-jammy|"
  "noble, 1 cpu|--cpus=1 $PW-noble|"
  "noble, 0.5 cpu, 1g ram|--cpus=0.5 --memory=1g $PW-noble|"
  "noble, --ipc=host|--ipc=host $PW-noble|"
  "noble, TZ+locale changed|-e TZ=Asia/Kolkata -e LANG=de_DE.UTF-8 $PW-noble|"
  "debian 12|node:22-bookworm|$INSTALL &&"
  "debian 12 + noto fonts|node:22-bookworm|$INSTALL && apt-get install -y fonts-noto-core >/dev/null 2>&1 &&"
)

echo "| case | dom | a11y | png | unique in-case (dom/a11y/png) |"
echo "|---|---|---|---|---|"
for c in "${cases[@]}"; do
  IFS='|' read -r name docker pre <<<"$c"
  [[ -n ${ONLY:-} && $name != *$ONLY* ]] && continue
  n=$N
  if [[ -z $docker ]]; then
    out=$(ALL_ONLY=1 node spike.mjs "$n" 2>&1)
  else
    # shellcheck disable=SC2086
    out=$(docker run --rm -v "$PWD":/spike:ro -w /spike -e ALL_ONLY=1 $docker \
      bash -c "$pre node spike.mjs $n" 2>&1)
  fi
  h=$(grep -o 'dom=.*' <<<"$out")
  row=$(grep '| all fixes' <<<"$out" | awk -F'|' '{print $3+0"/"$4+0"/"$5+0}')
  if [[ -z $h ]]; then echo "| $name | ERROR | | | $(tail -1 <<<"$out" | cut -c1-80) |"; continue; fi
  d=${h#dom=}; d=${d%% *}; a=${h#*a11y=}; a=${a%% *}; p=${h#*png=}
  echo "| $name | $d | $a | $p | $row |"
done
