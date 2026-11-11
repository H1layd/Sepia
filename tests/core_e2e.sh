#!/usr/bin/env bash
# Native integration test for the Rust data plane: chained SOCKS5/HTTP proxying,
# DoH held entirely inside the chain, blocklist, control API, clean shutdown.
#   bash tests/core_e2e.sh
set -uo pipefail

PASS=0
FAIL=0
ok()   { echo "  PASS  $*"; PASS=$((PASS + 1)); }
bad()  { echo "  FAIL  $*"; FAIL=$((FAIL + 1)); }
check_ok()   { if eval "$2" >/dev/null 2>&1; then ok "$1"; else bad "$1 ($2)"; fi; }
check_fail() { if eval "$2" >/dev/null 2>&1; then bad "$1 ($2)"; else ok "$1"; fi; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/core/target/release/sepia-core"
[ -x "$BIN" ] || { echo "build it first: cargo build --release --manifest-path core/Cargo.toml"; exit 1; }

TMP=$(mktemp -d)
trap 'kill ${CORE_PID:-} ${HOP_PID:-} 2>/dev/null; rm -rf "$TMP"' EXIT

BASE=$(( 42000 + ($$ * 7) % 3000 ))
HOP_SOCKS=$((BASE + 1))
HOP_CTRL=$((BASE + 2))
CORE_SOCKS=$((BASE + 3))
CORE_HTTP=$((BASE + 4))
CORE_CTRL=$((BASE + 5))
DNS_PORT=$((BASE + 6))

cat > "$TMP/hop.json" <<JSON
{
  "listen": {
    "socks": "127.0.0.1:$HOP_SOCKS",
    "http": "127.0.0.1:$((HOP_SOCKS + 10))",
    "redirect": "127.0.0.1:$((HOP_SOCKS + 11))",
    "dns": "127.0.0.1:$((HOP_SOCKS + 12))",
    "control": "127.0.0.1:$HOP_CTRL"
  },
  "chain": {"hops": []},
  "dns": {"enabled": false},
  "firewall": {"enabled": false}
}
JSON

cat > "$TMP/core.json" <<JSON
{
  "listen": {
    "socks": "127.0.0.1:$CORE_SOCKS",
    "http": "127.0.0.1:$CORE_HTTP",
    "control": "127.0.0.1:$CORE_CTRL"
  },
  "chain": {
    "hops": [{"type": "socks5", "addr": "127.0.0.1:$HOP_SOCKS", "label": "py-hop"}],
    "connect_timeout": 8
  },
  "dns": {
    "enabled": true,
    "listen": "127.0.0.1:$DNS_PORT",
    "servers": [{"url": "https://cloudflare-dns.com/dns-query",
                 "bootstraps": ["1.1.1.1", "1.0.0.1"]}],
    "block": ["ads.example"]
  },
  "rules": [{"domain": "blocked.test", "action": "block"}]
}
JSON

wait_health() {
  for _ in $(seq 1 100); do
    if curl -sS -m 1 "http://127.0.0.1:$1/health" >/dev/null 2>&1; then return 0; fi
    sleep 0.1
  done
  return 1
}

echo "== hop (python engine, serves as the upstream hop) =="
python3 -m sepia.cli --config "$TMP/hop.json" up --no-firewall --foreground > "$TMP/hop.log" 2>&1 &
HOP_PID=$!
if wait_health "$HOP_CTRL"; then ok "python hop is up on $HOP_SOCKS"; else bad "python hop failed"; tail -5 "$TMP/hop.log"; fi

echo "== sepia-core =="
"$BIN" --config "$TMP/core.json" > "$TMP/core.log" 2>&1 &
CORE_PID=$!
if wait_health "$CORE_CTRL"; then ok "sepia-core started (pid $CORE_PID)"; else bad "sepia-core died"; tail -5 "$TMP/core.log"; fi
sed 's/^/  | /' "$TMP/core.log"

echo "== proxying through the chain =="
check_ok "SOCKS5 -> https"         "curl -sS -m 20 --socks5-hostname 127.0.0.1:$CORE_SOCKS -o /dev/null https://example.com"
check_ok "HTTP CONNECT -> https"   "curl -sS -m 20 -x http://127.0.0.1:$CORE_HTTP -o /dev/null https://example.com"
check_ok "HTTP absolute-URI -> http" "curl -sS -m 20 -x http://127.0.0.1:$CORE_HTTP -o /dev/null http://example.com"
check_fail "blocks a ruled domain"  "curl -sS -m 10 --socks5-hostname 127.0.0.1:$CORE_SOCKS -o /dev/null http://blocked.test"

echo "== dns (DoH over the chain) =="
check_ok "resolves through DoH"  "dig +time=10 +tries=1 +short @127.0.0.1 -p $DNS_PORT example.com"
check_ok "resolves over tcp"     "dig +tcp +time=10 +tries=1 +short @127.0.0.1 -p $DNS_PORT example.com"
BLOCKED=$(dig +time=8 +tries=1 +short @127.0.0.1 -p $DNS_PORT ads.example 2>/dev/null)
if [ "$BLOCKED" = "0.0.0.0" ]; then ok "blocklist answers 0.0.0.0"; else bad "blocklist answer was '$BLOCKED'"; fi

echo "== control =="
STATUS=$(curl -sS -m 5 "http://127.0.0.1:$CORE_CTRL/status")
echo "$STATUS" | python3 -m json.tool | sed 's/^/  | /' | head -24
ENGINE=$(echo "$STATUS" | python3 -c "import json,sys;print(json.load(sys.stdin).get('engine'))")
CHAINED=$(echo "$STATUS" | python3 -c "import json,sys;print(json.load(sys.stdin)['counters']['chained'])")
BLOCKED_N=$(echo "$STATUS" | python3 -c "import json,sys;print(json.load(sys.stdin)['counters']['blocked'])")
ERRORS=$(echo "$STATUS" | python3 -c "import json,sys;print(len(json.load(sys.stdin)['counters']['errors']))")
[ "$ENGINE" = "rust" ] && ok "status reports the rust engine" || bad "engine tag was '$ENGINE'"
[ "${CHAINED:-0}" -ge 1 ] && ok "traffic went through the hop ($CHAINED chained)" || bad "chained counter stayed at zero"
[ "${BLOCKED_N:-0}" -ge 1 ] && ok "blocked counter moved ($BLOCKED_N)" || bad "blocked counter stayed at zero"
[ "${ERRORS:-1}" -eq 0 ] && ok "no connection errors" || bad "$ERRORS connection errors"

echo "== stop =="
kill "$CORE_PID" 2>/dev/null
sleep 0.5
if kill -0 "$CORE_PID" 2>/dev/null; then bad "still running"; else ok "stopped cleanly"; fi

echo
echo "== result: $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
