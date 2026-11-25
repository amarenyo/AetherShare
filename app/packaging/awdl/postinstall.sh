#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

if command -v nmcli >/dev/null 2>&1; then
  nmcli general reload conf >/dev/null 2>&1 || true
fi

if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules >/dev/null 2>&1 || true
fi

# Allow AirDrop traffic (mDNS + TCP 8771) on the isolated AWDL virtual interface
if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd --permanent --zone=trusted --add-interface=awdl0 >/dev/null 2>&1 || true
  firewall-cmd --reload >/dev/null 2>&1 || true
elif command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
  ufw allow in on awdl0 to any port 8771 proto tcp >/dev/null 2>&1 || true
  ufw allow in on awdl0 to any port 5353 proto udp >/dev/null 2>&1 || true
fi

exit 0
