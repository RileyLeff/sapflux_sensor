#!/bin/sh
# Run ON THE VPS as root, from inside the unpacked bundle directory:
#   tar xzf sapflux-x86_64.tar.gz && cd sapflux-x86_64 && sudo ./install.sh
# Idempotent: re-run to upgrade binaries/units. Never overwrites
# /etc/sapflux/server.toml or replicate.env once they exist.
set -eu
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }
cd "$(dirname "$0")"
UDP_PORT="${UDP_PORT:-47800}"

apt-get update -q
DEBIAN_FRONTEND=noninteractive apt-get install -y -q sqlite3 rsync ufw openssh-client

id sapflux >/dev/null 2>&1 || \
    useradd --system --home-dir /var/lib/sapflux --create-home --shell /usr/sbin/nologin sapflux
install -d -o sapflux -g sapflux -m 0750 /var/lib/sapflux
install -d -o root -g sapflux -m 0750 /etc/sapflux
install -d -m 0755 /usr/local/lib/sapflux /usr/local/share/doc/sapflux

# Binaries + helpers.
if systemctl is-active -q sapflux-server; then systemctl stop sapflux-server; RESTART=1; else RESTART=0; fi
install -m 0755 bin/sapflux-server bin/sapflux bin/sapflux-gateway-sim /usr/local/lib/sapflux/
install -m 0755 replicate.sh /usr/local/lib/sapflux/replicate.sh
install -m 0755 sapflux-cli-wrapper.sh /usr/local/bin/sapflux
install -m 0644 README.md config.example.toml /usr/local/share/doc/sapflux/

# Config (keys!) readable by the service user only.
if [ ! -e /etc/sapflux/server.toml ]; then
    install -o root -g sapflux -m 0640 config.example.toml /etc/sapflux/server.toml
    echo ">>> created /etc/sapflux/server.toml from the example: EDIT IT (device key)"
fi
[ -e /etc/sapflux/replicate.env ] || install -o root -g sapflux -m 0640 replicate.env.example /etc/sapflux/replicate.env

# SSH key the replication job uses to reach acacia.
if [ ! -e /var/lib/sapflux/.ssh/id_ed25519 ]; then
    install -d -o sapflux -g sapflux -m 0700 /var/lib/sapflux/.ssh
    sudo -u sapflux ssh-keygen -q -t ed25519 -N "" -C "sapflux@$(hostname)" -f /var/lib/sapflux/.ssh/id_ed25519
fi

# systemd.
install -m 0644 systemd/sapflux-server.service systemd/sapflux-replicate.service \
    systemd/sapflux-replicate.timer /etc/systemd/system/
systemctl daemon-reload
systemctl enable sapflux-server.service sapflux-replicate.timer >/dev/null

# Firewall: public SSH + the gateway's UDP port only. The tailnet interface
# is trusted (admin API, SSH over Tailscale).
ufw default deny incoming >/dev/null
ufw default allow outgoing >/dev/null
ufw allow OpenSSH >/dev/null
ufw allow "$UDP_PORT/udp" comment "sapflux gateway ingest" >/dev/null
if ip link show tailscale0 >/dev/null 2>&1; then ufw allow in on tailscale0 >/dev/null; fi
ufw --force enable >/dev/null
ufw status verbose

if grep -q '0000000000000000000000000000000000000000000000000000000000000000' /etc/sapflux/server.toml; then
    echo ">>> /etc/sapflux/server.toml still has the placeholder key; not starting."
    echo "    sapflux keygen --name box1 --device-id <ID>   then paste the [[device]] block"
    echo "    systemctl start sapflux-server"
else
    systemctl restart sapflux-server
    [ "$RESTART" = 1 ] && echo "restarted sapflux-server"
    systemctl --no-pager status sapflux-server | head -5
fi
systemctl start sapflux-replicate.timer
echo
echo "replication public key (append to acacia:~/.ssh/authorized_keys):"
cat /var/lib/sapflux/.ssh/id_ed25519.pub
