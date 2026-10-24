#!/bin/sh
# Consistent SQLite snapshot (sqlite3 .backup, safe while the server writes)
# plus the per-window files, pushed to the replica over the tailnet.
# Append-only: never deletes anything on the replica.
set -eu
DATA_DIR="${DATA_DIR:-/var/lib/sapflux}"
REPLICA="${REPLICA:-acacia:sapflux/}"
SSH_OPTS="${SSH_OPTS:--o BatchMode=yes -o StrictHostKeyChecking=accept-new -o ConnectTimeout=20}"

snap="$DATA_DIR/backup/sapflux.db"
mkdir -p "$DATA_DIR/backup"
sqlite3 "$DATA_DIR/sapflux.db" ".timeout 10000" ".backup '$snap.tmp'"
sqlite3 "$snap.tmp" "PRAGMA integrity_check;" | grep -qx ok
mv -f "$snap.tmp" "$snap"

# shellcheck disable=SC2086
rsync -a --partial -e "ssh $SSH_OPTS" "$snap" "$DATA_DIR/windows" "$REPLICA"
echo "replicated $(du -sh "$DATA_DIR/windows" | cut -f1) of windows + db to $REPLICA"
