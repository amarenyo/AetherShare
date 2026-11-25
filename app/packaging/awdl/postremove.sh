#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl stop aethershare-awdld.service >/dev/null 2>&1 || true
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

if command -v nmcli >/dev/null 2>&1; then
  nmcli general reload conf >/dev/null 2>&1 || true
fi

exit 0
