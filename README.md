# Sakuio-agent

Cross-platform monitoring agent (Linux, macOS, Windows, BSD). Uses **SakuLink** over a pluggable board transport; the default build uses WebSocket only in `sakuio-agent-ws`.

## Run (development)

```bash
cargo run -p sakuio-agent -- \
  --endpoint ws://127.0.0.1:8080/api/agent/link \
  --token "<token from POST /api/admin/clients>"

# or environment variables:
export SAKUIO_NODE_TOKEN="<token>"
export SAKUIO_BOARD_ENDPOINT="ws://127.0.0.1:8080/api/agent/link"
export SAKUIO_AGENT_ENABLE_GPU=true
export SAKUIO_AGENT_MEMORY_INCLUDE_CACHE=false
export SAKUIO_AGENT_MEMORY_REPORT_RAW_USED=false
cargo run -p sakuio-agent
```

Create a node token via the admin API or UI, then start the agent on the host.
