#!/bin/bash
# Self-test of the M15 harness parts that need neither root nor Lunix (run on the host: tools/remote.sh run bash
# tests/exit/selftest.sh): syntax of every script, the fio job files parse with the variables the scripts export, and
# the table generator and the xfstests comparison run on the fixtures. Prints PASS/FAIL lines; exit 1 on any FAIL.
here="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$here/../.." && pwd)"
FAIL=0
ck() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; FAIL=1; fi; }
for f in "$here"/*.sh; do bash -n "$f"; ck "bash -n $(basename "$f")" $?; done
for f in "$here"/*.py "$REPO"/bench/phase1/exit/*.py; do python3 -I -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" "$f"; ck "python syntax $(basename "$f")" $?; done
export DEV=/dev/null DIR=/tmp BASE=0 PER_JOB=134217728 JOBS=2 QD=8 RUNTIME=10 SEED=1 LOOPS=1 VONLY=0 TRIM_BASE=0 TRIM_SIZE=67108864
export BS=65536 SIZE=1g COMP=50 DEDUP=50 NUMJOBS=4
for j in "$REPO"/bench/phase1/exit/fio/*.fio; do fio --parse-only "$j" > /dev/null 2>&1; ck "fio --parse-only $(basename "$j")" $?; done
python3 -I "$REPO/bench/phase1/exit/summarize.py" "$here/fixtures/ratio-out" | grep -q 'dedup ok, compress ok'; ck "summarize.py on the ratio fixture" $?
python3 -I "$REPO/bench/phase1/exit/summarize.py" "$here/fixtures/ratio-out" | grep -q 'random stored raw ok'; ck "summarize.py: random data stored raw" $?
if command -v shellcheck > /dev/null; then shellcheck -x --source-path=SCRIPTDIR -S warning "$here"/*.sh; ck "shellcheck" $?; else echo "SKIP shellcheck (not installed here)"; fi
exit "$FAIL"
