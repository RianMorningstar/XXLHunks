#!/bin/sh
# Smoke test: a test server on 8721 with an empty data folder and the test clock, tests/smoke.py against it, then
# the server stopped. Uses the same settings as the server (.env or the environment); build first (cargo build
# --release).
cd "$(dirname "$0")/.."
# the same settings the server reads (.env, if there is one)
[ -f .env ] && { set -a; . ./.env; set +a; }
for i in $(seq 1 15); do curl -s -o /dev/null http://127.0.0.1:8721/ || break; sleep 1; done
rm -rf target/devdata
# the test server's own data folder and address, whatever the environment or .env says
export WOOFIA_DATA=target/devdata WOOFIA_ADDR=127.0.0.1:8721 WOOFIA_TEST_CLOCK=1 WOOFIA_REPLAY=
./target/release/woofia-server > target/dev-server.log 2>&1 &
PID=$!
for i in $(seq 1 30); do curl -s -o /dev/null http://127.0.0.1:8721/ && break; sleep 1; done
PYTHONIOENCODING=utf-8 ${PYTHON:-python3} tests/smoke.py > target/smoke.out 2>&1
kill $PID 2>/dev/null
echo "$(grep -c '^ok' target/smoke.out) passed, $(grep -c '^FAIL' target/smoke.out) failed"
grep -E "^FAIL" target/smoke.out | cut -c1-500
