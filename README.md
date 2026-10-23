# Sakuio Agent

Cross-platform monitoring agent (Linux, macOS, Windows, BSD). Uses **SakuLink** over a pluggable board transport; the default build uses WebSocket only in `sakuio-agent-ws`.

## 构建 / 运行

```bash
cargo run -p sakuio-agent -- \
  --endpoint ws://<中控地址> \
  --token "<token from POST /api/admin/clients>"

# or environment variables:
export SAKUIO_NODE_TOKEN="<token>"
export SAKUIO_BOARD_ENDPOINT="ws://127.0.0.1:8080"
export SAKUIO_AGENT_ENABLE_GPU=true
export SAKUIO_AGENT_MEMORY_INCLUDE_CACHE=false
export SAKUIO_AGENT_MEMORY_REPORT_RAW_USED=false
cargo run -p sakuio-agent
```

依赖主仓库的 `sakuio-protocol` / `sakuio-board`（相对路径 `../sakuio`）。
节点 token 通过管理端创建（「添加服务器」生成一键安装命令）。

## 安装选项

与一键安装命令一致，完整说明见主仓库 `docs/CICD.md`：

- `-e, --endpoint` 中控 WebSocket 地址
- `-t, --token`（别名 `--auto-discovery`）节点 token
- `--disable-remote-control` 禁用远程控制（终端 / 文件 / Shell / Exec）
- `--insecure` 跳过 TLS 证书校验
- `--include-buffer-memory` 内存统计包含缓冲区 / 缓存
- `--ip-from-nic` 优先从网卡获取 IP
- `--gpu-detail` 启用详细 GPU 监控
- `--include-nics` / `--exclude-nics` 网卡过滤（逗号分隔）
- `--include-mountpoints` 挂载点过滤（逗号分隔）
- `-i, --interval` 采集间隔（秒）
- `--monthly-reset-day` 网络统计月重置日

## 同机多实例

默认会自动选择空闲实例名和目录：

```text
sakuio-agent      /opt/sakuio-agent
sakuio-agent-2    /opt/sakuio-agent-2
sakuio-agent-3    /opt/sakuio-agent-3
```

也可以显式指定：

```bash
curl -fsSL https://raw.githubusercontent.com/sakuio-monitor/Sakuio-agent/main/install.sh \
  | sudo bash -s -- -e wss://panel-b.example.com -t <token> --name sakuio-agent-b
```

每个实例拥有独立的二进制、状态文件、systemd/OpenRC/launchd 服务，不会覆盖已有 Agent。

Windows：

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -e wss://panel-b.example.com -t <token> --name sakuio-agent-b
```

## Alpine / OpenRC

Alpine 需要先安装 bash 和 curl：

```bash
apk add --no-cache bash curl ca-certificates
curl -fsSL https://raw.githubusercontent.com/sakuio-monitor/Sakuio-agent/main/install.sh \
  | sudo bash -s -- -e wss://panel.example.com -t <token>
```

脚本会自动识别 OpenRC，并写入 `/etc/init.d/sakuio-agent[-N]`。

## 发布

推送 `v*` tag 自动交叉构建全部目标并发布 Release
（Linux amd64/386/arm64/arm/loong64/riscv64/mips/mipsel、Darwin、Windows，
详见 `.github/workflows/release.yml` 与主仓库 `docs/CICD.md`）。
