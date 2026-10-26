#!/usr/bin/env bash
# Installs the approved poller as the current user's systemd timer. Uses the
# user's existing gh credentials; no tokens are copied into the unit files.
set -euo pipefail
command -v gh >/dev/null
gh auth status >/dev/null
dir="$HOME/.local/lib/spark-rs-mirror"
units="$HOME/.config/systemd/user"
mkdir -p "$dir" "$units"
install -m 755 "$(dirname "$0")/poll.sh" "$dir/poll.sh"
cat > "$units/spark-rs-mirror.service" <<EOF
[Unit]
Description=Keep the spark-rs GitHub mirror current

[Service]
Type=oneshot
ExecStart=/bin/bash "%h/.local/lib/spark-rs-mirror/poll.sh"
Environment="PATH=$(dirname "$(command -v gh)"):/usr/local/bin:/usr/bin:/bin"
TimeoutStartSec=50
EOF
cat > "$units/spark-rs-mirror.timer" <<'EOF'
[Unit]
Description=Check the spark-rs mirror every minute

[Timer]
OnBootSec=60
OnUnitActiveSec=60
AccuracySec=5
Persistent=true

[Install]
WantedBy=timers.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now spark-rs-mirror.timer
