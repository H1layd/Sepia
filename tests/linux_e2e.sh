#!/usr/bin/env bash
# End-to-end validation of the Linux backend inside a container with NET_ADMIN.
# Proves: nftables loads, killswitch blocks direct egress, transparent redirect
# works, DNS is forced through the local resolver, and teardown restores network.
set -uo pipefail

PASS=0
FAIL=0

ok()   { echo "  PASS  $*"; PASS=$((PASS + 1)); }
bad()  { echo "  FAIL  $*"; FAIL=$((FAIL + 1)); }

check_ok()   { if eval "$2" >/dev/null 2>&1; then ok "$1"; else bad "$1 ($2)"; fi; }
check_fail() { if eval "$2" >/dev/null 2>&1; then bad "$1 ($2)"; else ok "$1"; fi; }

echo "== prep =="
mkdir -p /root/.sepia/run /root/.sepia/logs
rm -f /root/.sepia/run/*.json /root/.sepia/run/*.pid

cat > /root/hop.json <<'JSON'
{
  "listen": {
    "socks": "127.0.0.1:10800",
    "http": "127.0.0.1:18080",
    "redirect": "127.0.0.1:18081",
    "dns": "127.0.0.1:15353",
    "control": "127.0.0.1:19099"
  },
  "chain": {"hops": []},
  "dns": {"enabled": false},
  "firewall": {"enabled": false}
}
JSON

cat > /root/main.json <<'JSON'
{
  "listen": {
    "socks": "127.0.0.1:10810",
    "http": "127.0.0.1:18090",
    "redirect": "127.0.0.1:18091",
    "dns": "127.0.0.1:15453",
    "control": "127.0.0.1:19109"
  },
  "chain": {
    "hops": [{"type": "socks5", "addr": "127.0.0.1:10800", "label": "local"}],
    "health_url": "https://1.1.1.1/cdn-cgi/trace"
  },
  "firewall": {
    "enabled": true,
    "killswitch": true,
    "transparent": true,
    "block_ipv6": true
  }
}
JSON

python3 -m sepia.cli --config /root/hop.json up --no-firewall --foreground >/tmp/hop.log 2>&1 &
HOP=$!
sleep 2
if kill -0 "$HOP" 2>/dev/null; then ok "hop proxy started"; else bad "hop proxy died"; cat /tmp/hop.log; fi

echo "== baseline (before killswitch) =="
check_ok   "direct HTTPS works before lockdown" "curl -sS -m 10 -o /dev/null https://example.com"
check_ok   "external DNS works before lockdown"  "dig +time=4 +tries=1 +short @8.8.8.8 example.com"

echo "== apply =="
python3 -m sepia.cli --config /root/main.json up 2>&1 | sed 's/^/  | /'
if nft list table inet sepia >/dev/null 2>&1; then ok "nftables table installed"; else bad "nftables table missing"; fi

echo "== ruleset =="
nft list table inet sepia | sed 's/^/  | /'

echo "== killswitch =="
# A transparent redirect legitimately makes a "direct" request succeed; what
# matters is that it leaves through the chain and that non-redirected ports die.
check_fail "non-redirected port 853 denied"    "timeout 5 bash -c '</dev/tcp/8.8.8.8/853'"
check_fail "non-redirected port 4444 denied"   "timeout 5 bash -c '</dev/tcp/1.1.1.1/4444'"

EXIT_PROXY=$(curl -sS -m 15 -x http://127.0.0.1:18090 https://1.1.1.1/cdn-cgi/trace 2>/dev/null | awk -F= '/^ip=/{print $2}')
EXIT_DIRECT=$(curl -sS -m 15 --noproxy '*' https://1.1.1.1/cdn-cgi/trace 2>/dev/null | awk -F= '/^ip=/{print $2}')
echo "  exit via proxy : ${EXIT_PROXY:-none}"
echo "  exit transparent: ${EXIT_DIRECT:-none}"
if [ -n "$EXIT_PROXY" ] && [ "$EXIT_PROXY" = "$EXIT_DIRECT" ]; then
  ok "transparent traffic leaves through the chain"
else
  bad "transparent traffic exits elsewhere (leak)"
fi

DNS_BEFORE=$(python3 -c "import json,urllib.request;print(json.load(urllib.request.urlopen('http://127.0.0.1:19109/status'))['dns']['queries'])")
DIG_OUT=$(dig +time=4 +tries=1 +short @8.8.8.8 example.com 2>/dev/null)
DNS_AFTER=$(python3 -c "import json,urllib.request;print(json.load(urllib.request.urlopen('http://127.0.0.1:19109/status'))['dns']['queries'])")
if [ "$DNS_AFTER" -gt "$DNS_BEFORE" ] && [ -n "$DIG_OUT" ]; then
  ok "plaintext DNS intercepted by sepia ($DNS_BEFORE -> $DNS_AFTER queries)"
else
  bad "plaintext DNS was answered without sepia ($DNS_BEFORE -> $DNS_AFTER)"
fi

echo "== paths that must still work =="
check_ok "HTTP proxy -> internet"              "curl -sS -m 15 -x http://127.0.0.1:18090 -o /dev/null https://example.com"
check_ok "SOCKS5 proxy -> internet"            "curl -sS -m 15 --socks5-hostname 127.0.0.1:10810 -o /dev/null https://example.com"
check_ok "transparent HTTP (port 80)"          "curl -sS -m 15 --noproxy '*' -o /dev/null http://example.com"
check_ok "transparent HTTPS (original dst)"    "curl -sS -m 15 --noproxy '*' -o /dev/null https://example.com"
check_ok "local DNS resolver answers"          "dig +time=8 +tries=1 +short @127.0.0.1 -p 15453 example.com"
check_ok "local DNS over TCP answers"          "dig +tcp +time=8 +tries=1 +short @127.0.0.1 -p 15453 example.com"

echo "== counters =="
python3 -m sepia.cli --config /root/main.json status 2>&1 | sed -n '1,40p' | sed 's/^/  | /'

echo "== teardown =="
python3 -m sepia.cli --config /root/main.json down 2>&1 | sed 's/^/  | /'
kill "$HOP" 2>/dev/null
sleep 1
if nft list table inet sepia >/dev/null 2>&1; then bad "nftables table survived teardown"; else ok "nftables table removed"; fi
check_ok "direct HTTPS restored" "curl -sS -m 10 -o /dev/null https://example.com"

echo
echo "== result: $PASS passed, $FAIL failed =="
[ "$FAIL" -eq 0 ]
