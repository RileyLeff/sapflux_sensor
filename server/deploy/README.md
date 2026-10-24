# sapflux-server deployment (Hetzner VPS, Ubuntu 24.04)

The gateway (nRF9160, LTE-M) sends HMAC-signed UDP datagrams to this server
on **UDP 47800**. The admin API (no auth) listens on **127.0.0.1:8470** and is
never exposed publicly. Data lives in `/var/lib/sapflux` (SQLite `sapflux.db`
+ `windows/<BOARD>/run_<id>.{bin,csv,json}`) and is pushed to `acacia` over
Tailscale every 15 minutes.

Files here:

| file | goes to |
|---|---|
| `build.sh` | run on your laptop: static musl binaries + bundle |
| `install.sh` | run on the VPS as root: user, dirs, units, ufw |
| `systemd/sapflux-server.service` | `/etc/systemd/system/` (Restart=always) |
| `systemd/sapflux-replicate.{service,timer}` | `/etc/systemd/system/` |
| `replicate.sh` | `/usr/local/lib/sapflux/` (`sqlite3 .backup` + rsync) |
| `replicate.env.example` | `/etc/sapflux/replicate.env` |
| `sapflux-cli-wrapper.sh` | `/usr/local/bin/sapflux` (runs the CLI as user `sapflux`) |

## 1. Build (laptop)

```sh
cd server
deploy/build.sh            # x86_64 -> deploy/dist/sapflux-x86_64.tar.gz
deploy/build.sh aarch64    # if you pick an Arm (CAX) server
```

Uses `uvx` to run cargo-zigbuild + zig without installing them; binaries are
fully static (`file` prints "statically linked"). Alternative: install Rust on
the VPS and `cargo build --release` there.

## 2. Create the VPS (Hetzner Cloud console, you do this)

1. Add server: any location, **Ubuntu 24.04**, the cheapest shared-vCPU type
   (x86 "CX" line, or Arm "CAX" line; then build with `aarch64`).
2. Networking: keep **public IPv4** (the gateway uses an IPv4 literal).
3. Add your SSH public key. No volumes or backups needed.
4. Note the IPv4 address, e.g. `203.0.113.10`.

## 3. Tailscale (on the VPS)

```sh
ssh root@203.0.113.10
curl -fsSL https://tailscale.com/install.sh | sh
tailscale up --hostname sapflux-vps     # open the printed URL, approve
tailscale status                        # acacia should be listed
```

Do this before `install.sh` so ufw trusts the `tailscale0` interface.

## 4. Install

```sh
# laptop
scp server/deploy/dist/sapflux-x86_64.tar.gz root@203.0.113.10:
# VPS
tar xzf sapflux-x86_64.tar.gz && cd sapflux-x86_64 && ./install.sh
```

`install.sh` installs sqlite3/rsync/ufw, creates user `sapflux`, copies the
binaries to `/usr/local/lib/sapflux/`, creates `/etc/sapflux/server.toml`
(mode 0640 root:sapflux) from the example, enables the units, and sets ufw to
**deny incoming except OpenSSH + 47800/udp** (+ anything on `tailscale0`). It
does not start the server while the placeholder key is present. It prints the
replication SSH public key at the end.

## 5. Device key + config

```sh
sapflux keygen --name box1 --device-id <GATEWAY_DEVICE_ID>
```

Paste the `[[device]]` block into `/etc/sapflux/server.toml` (replace the
placeholder one), check `[defaults]` (`enabled = true` means "pulse"; the
board gets this schedule at the first check-in), then:

```sh
systemctl start sapflux-server
journalctl -u sapflux-server -f
```

Keep the keygen output; the gateway needs the same key.

## 6. Gateway config

| setting | value |
|---|---|
| server address | the VPS public IPv4 literal (`203.0.113.10`) |
| server port | `47800` (UDP) |
| device_id | the u64 you passed to keygen (`SAPFLUX_DEVICE_ID`) |
| key | `SAPFLUX_KEY_HEX` from keygen (32 bytes) |

Each new gateway boot must use a fresh random `boot_id` and restart `seq` at 1.
Retries must use a new `seq`, because the server drops replays without replying.

## 7. Replication to acacia

On **acacia**, append the key printed by `install.sh` (also in
`/var/lib/sapflux/.ssh/id_ed25519.pub` on the VPS) to `~/.ssh/authorized_keys`.
If your acacia username is not `sapflux`, set `REPLICA=you@acacia:sapflux/`
in `/etc/sapflux/replicate.env`. Then on the VPS:

```sh
systemctl start sapflux-replicate.service
journalctl -u sapflux-replicate -n 20      # "replicated ... to acacia:sapflux/"
systemctl list-timers sapflux-replicate.timer
```

acacia gets `~/sapflux/sapflux.db` (consistent snapshot) and
`~/sapflux/windows/`. The copy is append-only: nothing is ever deleted.

## 8. Verify end to end (laptop, before the gateway is live)

```sh
cd server
cargo run --release --bin sapflux-gateway-sim -- --server 203.0.113.10:47800 \
    --device-id <ID> --key <KEY_HEX> --no-window
ssh root@203.0.113.10 sapflux status
```

The sim reports a fake board; the real gateway's first check-in replaces it.

## Day-to-day (on the VPS)

```sh
sapflux off box1          # REMOTE OFF SWITCH (applied at next gateway check-in)
sapflux on box1
sapflux status            # last check-in, battery, board FieldStatus, schedule sync
sapflux schedule box1 --interval-s 1800 --phase-s 900
sapflux reboot-gateway box1
sapflux windows
sapflux export D15C55B4116E105E <RUN_ID> -o run.csv
sapflux events box1
journalctl -u sapflux-server --since -1h
```

Changes take effect when the gateway next checks in (every `wake_interval_s`,
default 3600 s). `status` shows "board sync" until the board reports the new
schedule CRC.

To use the admin API from your laptop:
`ssh -L 8470:127.0.0.1:8470 root@sapflux-vps`, then
`curl localhost:8470/api/devices`. Or set `api_bind = "<tailscale IP>:8470"`.
Never bind it to `0.0.0.0`.

Optional hardening: once Tailscale SSH access works, run `ufw delete allow OpenSSH`
so SSH is reachable only over the tailnet. The Hetzner web console still works
if you lock yourself out.

## Upgrade

Rebuild, scp the bundle, re-run `./install.sh`. It restarts the service and
never overwrites `server.toml` or `replicate.env`.
