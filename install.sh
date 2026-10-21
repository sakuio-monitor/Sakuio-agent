#!/usr/bin/env bash
# Sakuio agent one-line installer.
# Sakuio agent 一键安装脚本。
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sakuio-monitor/Sakuio-agent/main/install.sh \
#     | sudo bash -s -- -e wss://panel.example.com -t <node-token> [options...]
#
# Options:
#   -e, --endpoint URL            Board base URL, ws(s)://host[:port] (required)
#   -t, --token TOKEN             Node token (or auto-discovery key) (required)
#       --disable-remote-control  Disable terminal / file / shell / exec
#       --disable-auto-update     Disable built-in auto update (reserved)
#       --insecure                Skip TLS certificate verification
#       --include-buffer-memory   Include buffer/cache memory in usage
#       --ip-from-nic             Prefer NIC address as the reported IP
#       --gpu-detail              Enable detailed GPU monitoring
#       --version V               Install a specific agent version (e.g. 0.1.0)
#       --github-proxy URL        Download mirror prefix for GitHub assets
#       --install-dir DIR         Install directory (default /opt/sakuio-agent)
#       --service-name NAME       Service name (default sakuio-agent)
#       --include-nics LIST       Comma-separated NICs to include
#       --exclude-nics LIST       Comma-separated NICs to exclude
#       --include-mountpoints LIST  Comma-separated mount points to include
#   -i, --interval SECONDS        Telemetry interval (default 3)
#       --monthly-reset-day DAY   Day of month to rotate network counters
#
# 卸载：bash install.sh --uninstall [--service-name NAME] [--install-dir DIR]

set -euo pipefail

SERVICE_NAME="sakuio-agent"
INSTALL_DIR="/opt/sakuio-agent"
GITHUB_PROXY=""
AGENT_VERSION=""
UNINSTALL=0

ENDPOINT=""
TOKEN=""
EXTRA_ARGS=()

usage() { sed -n '2,30p' "$0"; exit 0; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    -e|--endpoint) ENDPOINT="$2"; shift 2 ;;
    -t|--token|--auto-discovery) TOKEN="$2"; shift 2 ;;
    --disable-remote-control) EXTRA_ARGS+=("--disable-remote-control"); shift ;;
    --disable-auto-update) EXTRA_ARGS+=("--disable-auto-update"); shift ;;
    --insecure) EXTRA_ARGS+=("--insecure"); shift ;;
    --include-buffer-memory) EXTRA_ARGS+=("--include-buffer-memory"); shift ;;
    --ip-from-nic) EXTRA_ARGS+=("--ip-from-nic"); shift ;;
    --gpu-detail) EXTRA_ARGS+=("--gpu-detail"); shift ;;
    --version) AGENT_VERSION="$2"; shift 2 ;;
    --github-proxy) GITHUB_PROXY="$2"; shift 2 ;;
    --install-dir) INSTALL_DIR="$2"; shift 2 ;;
    --service-name) SERVICE_NAME="$2"; shift 2 ;;
    --include-nics) EXTRA_ARGS+=("--include-nics" "$2"); shift 2 ;;
    --exclude-nics) EXTRA_ARGS+=("--exclude-nics" "$2"); shift 2 ;;
    --include-mountpoints) EXTRA_ARGS+=("--include-mountpoints" "$2"); shift 2 ;;
    -i|--interval) EXTRA_ARGS+=("--interval" "$2"); shift 2 ;;
    --monthly-reset-day) EXTRA_ARGS+=("--monthly-reset-day" "$2"); shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

# ---- uninstall path ----
if [[ "$UNINSTALL" -eq 1 ]]; then
  if command -v systemctl >/dev/null 2>&1; then
    systemctl disable --now "$SERVICE_NAME" 2>/dev/null || true
    rm -f "/etc/systemd/system/$SERVICE_NAME.service"
    systemctl daemon-reload 2>/dev/null || true
  elif [[ "$(uname)" == "Darwin" ]]; then
    launchctl bootout "system/$SERVICE_NAME" 2>/dev/null || \
      launchctl unload "/Library/LaunchDaemons/$SERVICE_NAME.plist" 2>/dev/null || true
    rm -f "/Library/LaunchDaemons/$SERVICE_NAME.plist"
  fi
  rm -rf "$INSTALL_DIR"
  echo "Sakuio agent uninstalled."
  exit 0
fi

if [[ -z "$ENDPOINT" || -z "$TOKEN" ]]; then
  echo "error: --endpoint and --token are required" >&2
  exit 1
fi

# ---- platform detection ----
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64) ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  armv7*|armv6*|arm) ARCH="arm" ;;
  i386|i486|i586|i686) ARCH="386" ;;
  loongarch64) ARCH="loong64" ;;
  riscv64) ARCH="riscv64" ;;
  mips64el) ARCH="mipsel" ;;
  mips64) ARCH="mips" ;;
  mipsel) ARCH="mipsel" ;;
  mips) ARCH="mips" ;;
  *) echo "error: unsupported architecture: $ARCH" >&2; exit 1 ;;
esac
case "$OS" in
  Linux) OS="linux" ;;
  Darwin) OS="darwin" ;;
  *) echo "error: unsupported OS: $OS" >&2; exit 1 ;;
esac

# ---- resolve latest version when not pinned ----
if [[ -z "$AGENT_VERSION" ]]; then
  LATEST_URL="${GITHUB_PROXY:-https://github.com}/sakuio-monitor/Sakuio-agent/releases/latest"
  if command -v curl >/dev/null 2>&1; then
    AGENT_VERSION="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$LATEST_URL" | sed 's|.*/tag/v||')"
  else
    AGENT_VERSION="$(wget -qO- --max-redirs=5 "$LATEST_URL" 2>/dev/null | sed 's|.*/tag/v||' || true)"
  fi
  if [[ -z "$AGENT_VERSION" || "$AGENT_VERSION" == *"$LATEST_URL"* ]]; then
    echo "error: failed to resolve latest version; pass --version" >&2
    exit 1
  fi
fi

ASSET="sakuio-agent-${OS}-${ARCH}"
DOWNLOAD_URL="${GITHUB_PROXY:-https://github.com}/sakuio-monitor/Sakuio-agent/releases/download/v${AGENT_VERSION}/${ASSET}"

echo "==> downloading ${DOWNLOAD_URL}"
mkdir -p "$INSTALL_DIR"
BIN="$INSTALL_DIR/sakuio-agent"
if command -v curl >/dev/null 2>&1; then
  curl -fL --retry 3 -o "$BIN.tmp" "$DOWNLOAD_URL"
elif command -v wget >/dev/null 2>&1; then
  wget -qO "$BIN.tmp" "$DOWNLOAD_URL"
else
  echo "error: curl or wget is required" >&2
  exit 1
fi
mv "$BIN.tmp" "$BIN"
chmod +x "$BIN"

echo "==> installing agent v${AGENT_VERSION} (${OS}/${ARCH}) to $INSTALL_DIR"

# ---- service definition ----
# The persisted token (auto discovery result) lives here; always pass it as
# an absolute path and pin the working directory, otherwise the default
# relative --state path lands wherever the service manager happens to start.
# 持久化令牌存放在安装目录；必须显式传绝对路径并固定工作目录，
# 否则默认相对路径会随服务管理器的启动目录漂移。
STATE_FILE="$INSTALL_DIR/sakuio-agent-token"
if command -v systemctl >/dev/null 2>&1 && [[ "$(uname -s)" == "Linux" ]]; then
  EXEC_LINE="$BIN -e \"$ENDPOINT\" -t \"$TOKEN\" --state \"$STATE_FILE\""
  for a in "${EXTRA_ARGS[@]:-}"; do
    [[ -n "$a" ]] && EXEC_LINE="$EXEC_LINE \"$a\""
  done
  cat > "/etc/systemd/system/$SERVICE_NAME.service" <<UNIT
[Unit]
Description=Sakuio monitoring agent
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$INSTALL_DIR
ExecStart=$EXEC_LINE
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
UNIT
  systemctl daemon-reload
  systemctl enable --now "$SERVICE_NAME"
  echo "==> service '$SERVICE_NAME' started (systemd)"
  systemctl --no-pager -l status "$SERVICE_NAME" | head -n 8 || true
elif [[ "$(uname -s)" == "Darwin" ]]; then
  PLIST="/Library/LaunchDaemons/$SERVICE_NAME.plist"
  ARGS_PLIST=""
  ARGS_PLIST="$ARGS_PLIST
    <string>$BIN</string>
    <string>-e</string><string>$ENDPOINT</string>
    <string>-t</string><string>$TOKEN</string>
    <string>--state</string><string>$STATE_FILE</string>"
  for a in "${EXTRA_ARGS[@]:-}"; do
    [[ -n "$a" ]] && ARGS_PLIST="$ARGS_PLIST
    <string>$a</string>"
  done
  cat > "$PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>$SERVICE_NAME</string>
    <key>ProgramArguments</key>
    <array>$ARGS_PLIST
    </array>
    <key>WorkingDirectory</key><string>$INSTALL_DIR</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/var/log/$SERVICE_NAME.log</string>
    <key>StandardErrorPath</key><string>/var/log/$SERVICE_NAME.log</string>
  </dict>
</plist>
PLIST
  launchctl bootout "system/$SERVICE_NAME" 2>/dev/null || true
  launchctl bootstrap system "$PLIST"
  echo "==> service '$SERVICE_NAME' started (launchd)"
else
  echo "error: neither systemd nor macOS detected; run the agent manually:" >&2
  echo "  $BIN -e '$ENDPOINT' -t '$TOKEN'" >&2
  exit 1
fi

echo "==> done. The node should appear online in the admin console shortly."
