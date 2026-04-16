# QA — hathor-mcp-stack

End-to-end test plan for the deployed stack. Every step is a `curl` you
can paste into a shell. Expected outcomes are listed after each.

Deployed endpoints:
- Orchestrator: `https://orchestrator.hathor.dev`
- MCP:          `https://mcp.hathor.dev`

Both point at `node1.playground.testnet.hathor.network` (testnet-playground).

## 0. Prerequisites

- `curl`, `python3` (only used for pretty-printing)
- A playground-testnet HTR address with some HTR (to fund a test wallet).
  The [playground faucet](https://playground.testnet.hathor.network/) issues test HTR.

Set up helpers used throughout this doc:

```bash
MCP=https://mcp.hathor.dev/mcp
ORCH=https://orchestrator.hathor.dev

tool() {
  # $1 = tool name, $2 = args JSON (default {})
  curl -s -X POST "$MCP" -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":${2:-\{\}}}}"
}
extract() {
  python3 -c "import json,sys; print(json.load(sys.stdin)['result']['content'][0]['text'])"
}
```

---

## 1. Smoke tests (no auth, no state)

### 1.1 Service health

```bash
curl -s "$ORCH/health"              # expect: OK
curl -s "$MCP/../health"            # expect: OK (path is /health, not /mcp/health)
curl -s "https://mcp.hathor.dev/health"  # explicit form
```

### 1.2 Orchestrator session list (initially)

```bash
curl -s "$ORCH/sessions"
# expect: {"sessions":[...]}  — may be empty or contain the MCP's auto-provisioned session
```

---

## 2. MCP protocol tests

### 2.1 `initialize`

```bash
curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","clientInfo":{"name":"qa","version":"0"}}}' \
  | python3 -m json.tool
```
Expect: `result.serverInfo.name == "hathor-mcp"`, `protocolVersion == "2024-11-05"`.

### 2.2 `tools/list`

```bash
curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}' \
  | python3 -c "import json,sys; [print(t['name']) for t in json.load(sys.stdin)['result']['tools']]"
```
Expect: 24 tool names (node/wallet/blockchain/blueprint/nano-contract/service-urls).

### 2.3 `ping`

```bash
curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}'
# expect: {"jsonrpc":"2.0","id":1,"result":{}}
```

### 2.4 `notifications/*` returns 204

```bash
curl -s -o /dev/null -w "%{http_code}\n" -X POST "$MCP" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}'
# expect: 204
```

### 2.5 Unknown method → `-32601`

```bash
curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"bogus/method","params":{}}'
# expect: error.code == -32601
```

---

## 3. Node & service introspection

### 3.1 `get_node_status`

```bash
tool get_node_status | extract | python3 -m json.tool | head -20
```
Expect: `running:true`, `status.server.app_version:"Hathor v0.69.0"`,
`status.server.network:"testnet-playground"`.

### 3.2 `get_service_urls`

```bash
tool get_service_urls | extract
```
Expect: `wallet_headless_url` points at `http://orchestrator:8100/sessions/<uuid>/api`
(auto-provisioned by the MCP on first wallet call — may be a stale URL until
you trigger a wallet op).

### 3.3 `set_service_urls` (runtime reconfig)

```bash
tool set_service_urls '{"tx_mining_url":"http://localhost:8002"}' | extract
```
Expect: `updated:true` with the new URL echoed back.

---

## 4. Wallet lifecycle — 3 wallets via MCP

All three wallets live inside the MCP's single auto-provisioned orchestrator
session. This exercises wallet isolation *within* a single wallet-headless
container.

### 4.1 Generate a seed (without creating a wallet)

```bash
tool generate_seed | extract
```
Expect: 24 lowercase BIP39 words.

### 4.2 Create three wallets

```bash
tool create_wallet '{"wallet_id":"alice"}' | extract
tool create_wallet '{"wallet_id":"bob"}'   | extract
tool create_wallet '{"wallet_id":"carol"}' | extract
```
Expect: `success:true`, `seed_stored:true` for each.

### 4.3 Wait for Ready, then check status

```bash
sleep 20
for w in alice bob carol; do
  tool get_wallet_status "{\"wallet_id\":\"$w\"}" | extract \
    | python3 -c "import json,sys; d=json.loads(sys.stdin.read()); print(f\"$w: {d['statusMessage']}\")"
done
```
Expect: `alice: Ready`, `bob: Ready`, `carol: Ready` (statusCode 3).

### 4.4 Seed retrieval

```bash
tool get_wallet_seed '{"wallet_id":"alice"}' | extract          # expect seed JSON
tool get_wallet_seed '{"wallet_id":"ghost"}' | extract          # expect "Seed not found" error
```

### 4.5 Addresses

```bash
tool get_wallet_addresses '{"wallet_id":"alice"}' | extract \
  | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['addresses'][0])"
```
Expect: a base58 address starting with `W`.

### 4.6 Balances (initially zero)

```bash
for w in alice bob carol; do
  echo -n "$w: "; tool get_wallet_balance "{\"wallet_id\":\"$w\"}" | extract
done
```
Expect: `{"available":0,"locked":0}` for all.

### 4.7 Fund Alice

Send HTR (e.g. 10 HTR) to Alice's first address using an external wallet or
the playground faucet. Then wait for confirmation:

```bash
for i in $(seq 1 6); do
  BAL=$(tool get_wallet_balance '{"wallet_id":"alice"}' | extract)
  echo "$i: $BAL"
  [[ "$BAL" != *'"available":0'* ]] && break
  sleep 10
done
```
Expect: `available > 0` within a few polls. Balance is in **cents**
(100 cents = 1 HTR).

### 4.8 Transfer HTR between wallets

```bash
BOB_ADDR=$(tool get_wallet_addresses '{"wallet_id":"bob"}' | extract \
  | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['addresses'][0])")
CAROL_ADDR=$(tool get_wallet_addresses '{"wallet_id":"carol"}' | extract \
  | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['addresses'][0])")

tool send_from_wallet "{\"wallet_id\":\"alice\",\"address\":\"$BOB_ADDR\",\"amount\":5}"   | extract
tool send_from_wallet "{\"wallet_id\":\"alice\",\"address\":\"$CAROL_ADDR\",\"amount\":3}" | extract

sleep 10
for w in alice bob carol; do
  echo -n "$w: "; tool get_wallet_balance "{\"wallet_id\":\"$w\"}" | extract
done
```
Expect: Alice decreases by 8 HTR, Bob = 500 (5 HTR), Carol = 300 (3 HTR).

**If `send_from_wallet` returns `"full validation failed: ..."`** — the
orchestrator was started without `--tx-mining-url` pointing at the playground
tx-mining service. Verify the orchestrator compose args include
`--tx-mining-url=https://txmining.playground.testnet.hathor.network/`.

### 4.9 Error paths

```bash
# Insufficient funds:
tool send_from_wallet '{"wallet_id":"carol","address":"'"$BOB_ADDR"'","amount":999}' | extract
# expect: {"success":false,"error":"Token: 00. Insufficient amount of tokens to fill the amount."}

# Invalid tool name:
tool nonexistent_tool '{}' | extract                     # expect: "Error: Unknown tool: nonexistent_tool"
```

### 4.10 Close wallets

```bash
tool close_wallet '{"wallet_id":"bob"}'   | extract       # {"success":true}
tool close_wallet '{"wallet_id":"carol"}' | extract       # {"success":true}
tool get_wallet_balance '{"wallet_id":"bob"}' | extract   # {"success":false,"message":"Invalid wallet id parameter."}
```

---

## 5. Blockchain queries

### 5.1 `get_blocks`

```bash
tool get_blocks '{"count":3}' | extract | python3 -m json.tool | head -30
```
Expect: an array of 3 recent blocks with `height`, `tx_id`, `outputs`.

### 5.2 `get_transaction`

Use a `tx_id` from the transfer in 4.8:

```bash
tool get_transaction '{"tx_id":"<tx_id_from_4.8>"}' | extract | python3 -m json.tool | head -30
```
Expect: `success:true`, full tx object with inputs/outputs.

Error path:

```bash
tool get_transaction '{"tx_id":"deadbeef"}' | extract
# expect: {"success":false,"message":"Transaction not found"}
```

---

## 6. Nano contracts (blueprints)

### 6.1 Publish a blueprint

Use the TicTacToe blueprint from `x402-poc/blueprint/tic_tac_toe.py` (or any
`@export class X(Blueprint)` Python file):

```bash
ALICE_ADDR=$(tool get_wallet_addresses '{"wallet_id":"alice"}' | extract \
  | python3 -c "import json,sys; print(json.loads(sys.stdin.read())['addresses'][0])")
CODE=$(python3 -c "import json; print(json.dumps(open('PATH/TO/tic_tac_toe.py').read()))")

curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"publish_blueprint\",\"arguments\":{\"wallet_id\":\"alice\",\"address\":\"$ALICE_ADDR\",\"code\":$CODE}}}" \
  | extract | python3 -c "import json,sys; print('blueprint_id:', json.loads(sys.stdin.read())['hash'])"
```
Expect: `blueprint_id` hash (version 6 tx). Save it as `$BP`.

### 6.2 Instantiate a nano contract

```bash
BP=<blueprint_id_from_6.1>
tool create_nano_contract "{\"wallet_id\":\"alice\",\"blueprint_id\":\"$BP\",\"address\":\"$ALICE_ADDR\",\"args\":[100]}" \
  | extract | python3 -c "import json,sys; print('nc_id:', json.loads(sys.stdin.read())['hash'])"
```
Expect: `nc_id` hash. Save it as `$NC`. The `args` vary per blueprint —
TicTacToe takes a `bet_amount` (int, in cents).

### 6.3 Execute methods with actions

Two players join, each depositing 100 cents:

```bash
NC=<nc_id_from_6.2>
tool execute_nano_contract "{\"wallet_id\":\"bob\",\"nc_id\":\"$NC\",\"method\":\"join\",\"address\":\"$BOB_ADDR\",\"args\":[],\"actions\":[{\"type\":\"deposit\",\"token\":\"00\",\"amount\":100}]}" | extract
tool execute_nano_contract "{\"wallet_id\":\"carol\",\"nc_id\":\"$NC\",\"method\":\"join\",\"address\":\"$CAROL_ADDR\",\"args\":[],\"actions\":[{\"type\":\"deposit\",\"token\":\"00\",\"amount\":100}]}" | extract

# Play a move (Bob plays position 0)
tool execute_nano_contract "{\"wallet_id\":\"bob\",\"nc_id\":\"$NC\",\"method\":\"play\",\"address\":\"$BOB_ADDR\",\"args\":[0]}" | extract
```
Expect: `success:true` for each (each is a new confirmed-pending tx).

### 6.4 State, history, logs

Wait ~60s for block confirmation:

```bash
sleep 60
tool get_nano_contract_state   "{\"nc_id\":\"$NC\"}" | extract | python3 -m json.tool
tool get_nano_contract_history "{\"nc_id\":\"$NC\"}" | extract | python3 -m json.tool | head -40
tool get_nano_contract_logs    "{\"tx_id\":\"$NC\"}" | extract | python3 -m json.tool | head -20
```
Expect:
- `get_nano_contract_state` → `blueprint_name:"TicTacToe"` (field/balance
  maps may be sparse depending on blueprint).
- `get_nano_contract_history` → array of NC txs (init + joins + plays).
- `get_nano_contract_logs` → `nc_execution:"success"` with CALL_BEGIN /
  CALL_END entries for each invocation.

### 6.5 Error paths

```bash
tool get_nano_contract_state '{"nc_id":"deadbeef"}' | extract
# expect: "Nano contract does not exist at block ..."
```

---

## 7. Orchestrator API — session isolation

Verify that the orchestrator can spawn multiple **isolated** wallet-headless
containers, each with its own wallet.

### 7.1 Create 3 isolated sessions

```bash
for i in 1 2 3; do
  curl -s -X POST "$ORCH/sessions" | python3 -c "import json,sys; d=json.load(sys.stdin); print(f\"session {d['session_id']} key {d['api_key']}\")"
done
```
Each call returns a fresh `session_id` UUID and `api_key`. Save the session IDs.

### 7.2 List sessions

```bash
curl -s "$ORCH/sessions" | python3 -m json.tool
```
Expect: all session IDs listed with per-session `port` and `idle_secs`.

### 7.3 Start a wallet in each session

```bash
# Generate valid seeds via MCP:
SEED1=$(curl -s -X POST "$MCP" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"generate_seed","arguments":{}}}' \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['result']['content'][0]['text'])")
# (repeat for SEED2, SEED3)

SID1=<session-1-uuid>
curl -s -X POST "$ORCH/sessions/$SID1/api/start" \
  -H "Content-Type: application/json" \
  -d "{\"wallet-id\":\"w\",\"seed\":\"$SEED1\"}"
# expect: {"success":true}
```
Repeat for sessions 2 and 3 with their respective seeds.

The orchestrator transparently injects the session's `x-api-key` header when
proxying; you do NOT need to send it from your side.

### 7.4 Verify isolation — different addresses per session

```bash
sleep 20
for SID in $SID1 $SID2 $SID3; do
  curl -s "$ORCH/sessions/$SID/api/wallet/address?index=0" \
    -H "x-wallet-id: w"
done
```
Expect: three different base58 addresses (one per container).

### 7.5 Destroy sessions

```bash
for SID in $SID1 $SID2 $SID3; do
  curl -s -X DELETE "$ORCH/sessions/$SID"
done
# each: {"destroyed":true,"session_id":"..."}
```

### 7.6 Destroy error path

```bash
curl -s -w "\nHTTP %{http_code}\n" -X DELETE "$ORCH/sessions/does-not-exist"
# expect: {"error":"Session not found"} HTTP 404
```

---

## 8. Known limitations

These are **not** bugs in the stack; they reflect the upstream
`node1.playground.testnet.hathor.network` configuration.

| Tool                    | Status | Reason                                        |
| ----------------------- | ------ | --------------------------------------------- |
| `list_blueprints`       | 403    | Playground nginx blocks `/nano_contract/blueprint` listing |
| `get_blueprint_info`    | 403    | Same as above — `/nano_contract/blueprint/info` blocked |
| `get_faucet_balance`    | 403    | Playground fullnode not started with `--wallet`; the `/thin_wallet/...` path is blocked |
| `send_from_faucet`      | 403    | Same — no built-in faucet on the playground fullnode |
| `fund_wallet`           | Error  | Fails because it depends on `get_faucet_balance` |

To test `list_blueprints` / `get_blueprint_info` / the faucet tools, point
the MCP at a fullnode that exposes those endpoints (e.g. a local
`hathor-core` with `--wallet`).

---

## 9. Checklist summary

### Orchestrator (HTTP)
- [ ] `GET /health` → 200 OK
- [ ] `GET /sessions` — list
- [ ] `POST /sessions` × 3 — each returns unique `session_id` + `api_key`
- [ ] `POST /sessions/:id/api/start` — wallet starts inside isolated container
- [ ] `GET /sessions/:id/api/wallet/address` — addresses differ across sessions
- [ ] `DELETE /sessions/:id` — destroys container
- [ ] `DELETE /sessions/bogus` → 404

### MCP protocol
- [ ] `initialize` returns server info
- [ ] `tools/list` returns 24 tools
- [ ] `ping` → `{}`
- [ ] `notifications/*` → 204
- [ ] Unknown method → JSON-RPC `-32601`

### MCP tools
- [ ] `get_node_status` — testnet-playground, v0.69.0
- [ ] `get_service_urls` — reports orchestrator-backed URL
- [ ] `set_service_urls` — updates URLs at runtime
- [ ] `generate_seed` — 24-word BIP39
- [ ] `create_wallet` × 3 (alice/bob/carol)
- [ ] `get_wallet_status` × 3 — all Ready
- [ ] `get_wallet_seed` — round-trips stored seed; unknown wallet → error
- [ ] `get_wallet_addresses` × 3 — each returns distinct set
- [ ] `get_wallet_balance` — zero initially
- [ ] `send_from_wallet` — transfer succeeds after Alice is funded
- [ ] `send_from_wallet` insufficient funds → proper error
- [ ] `close_wallet` — removes wallet; post-close balance call fails cleanly
- [ ] `get_blocks` — returns block array
- [ ] `get_transaction` — full tx; bad ID → "Transaction not found"
- [ ] `publish_blueprint` — returns version-6 blueprint hash
- [ ] `create_nano_contract` — returns nc_id hash
- [ ] `execute_nano_contract` with deposit/withdrawal actions
- [ ] `get_nano_contract_state` — blueprint_name correct
- [ ] `get_nano_contract_history` — lists all txs
- [ ] `get_nano_contract_logs` — CALL_BEGIN/CALL_END entries
- [ ] Invalid tool name → `Error: Unknown tool: ...`

### Known 403s (upstream, not stack bugs)
- [ ] `list_blueprints` — 403 from playground nginx
- [ ] `get_blueprint_info` — 403 from playground nginx
- [ ] `get_faucet_balance` / `send_from_faucet` / `fund_wallet` — faucet not available on playground fullnode
