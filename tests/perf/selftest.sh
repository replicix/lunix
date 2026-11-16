#!/bin/bash
# Self-test of the M16 parts that need neither root nor Lunix (run on the host: tools/remote.sh run bash tests/perf/selftest.sh):
# syntax of the scripts, fio --parse-only of every job file, and both analysers on synthetic fixtures.
here="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$here/../.." && pwd)"
FAIL=0
ck() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; FAIL=1; fi; }
for f in "$here"/*.sh; do bash -n "$f"; ck "bash -n $(basename "$f")" $?; done
for f in "$here"/*.py "$REPO"/bench/phase1/perf/*.py "$REPO"/bench/phase1/chunksize/*.py; do python3 -I -c "import ast,sys; ast.parse(open(sys.argv[1]).read())" "$f"; ck "python syntax $(basename "$f")" $?; done
export DEV=/dev/null DIR=/tmp WS=4294967296 SLICE=536870912 RUNTIME=10 RAMP=2 CPUS=0-3 JOBS=8 QD=32 SEED=1 SIZE=1g
for j in "$REPO"/bench/phase1/perf/fio/*.fio "$REPO"/bench/phase1/chunksize/fio/*.fio; do fio --parse-only "$j" > /dev/null 2>&1; ck "fio --parse-only $(basename "$j")" $?; done
T=$(mktemp -d)
python3 -I "$here/make-fixtures.py" "$T/pa" "$T/ck"; ck "fixtures" $?
python3 -I "$REPO/bench/phase1/perf/analyze.py" "$T/pa" --json "$T/verdict.json" > "$T/pa.md"; ck "perf analyze.py" $?
grep -q 'SHORTFALL' "$T/pa.md"; ck "perf analyze.py reports a shortfall row (w4k is below its target in the fixture)" $?
grep -q 'verify_reads = true' "$T/pa.md"; ck "perf analyze.py has the verify_reads table" $?
python3 -I "$REPO/bench/phase1/chunksize/analyze.py" "$T/ck" --parta "$T/verdict.json" > "$T/ck.md"; ck "chunksize analyze.py" $?
grep -q 'Recommendation by the rule' "$T/ck.md"; ck "chunksize analyze.py evaluates the rule" $?
rm -rf "$T"
if command -v shellcheck > /dev/null; then shellcheck -x --source-path=SCRIPTDIR -S warning "$here"/*.sh; ck "shellcheck" $?; else echo "SKIP shellcheck (not installed here)"; fi
exit "$FAIL"
