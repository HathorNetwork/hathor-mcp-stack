# hathor-mcp-stack

Deployment bundle for the Hathor MCP server and the multi-tenant
wallet-headless orchestrator. Runs as a single docker-compose.

## Services

- **orchestrator** (port 8100) — [HathorNetwork/headless-orchestrator](https://github.com/HathorNetwork/headless-orchestrator). Spawns per-session `hathor-wallet-headless` containers via the host Docker socket.
- **mcp** (port 9876) — [HathorNetwork/hathor-mcp](https://github.com/HathorNetwork/hathor-mcp). MCP server, configured to use the orchestrator for isolated wallets.

Both point at `node1.playground.testnet.hathor.network` by default.

## Networking

A named docker network `hathor-mcp-stack` is created by compose. The
orchestrator attaches sibling containers (spawned via the Docker
socket) to this same network so the proxy can reach them by container
name — no host-port plumbing required.

## Deploying

Submodules track the `feat/deployment` branch of each source repo.
After pulling updates upstream, bump the submodule pointers:

```bash
git submodule update --remote
git commit -am "deps: bump submodules"
```

Dokploy is configured to initialize submodules on clone
(`enableSubmodules=true`).
