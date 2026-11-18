# Installation

alogeno is a single binary that runs as a systemd service on a Linux server. The installer downloads a release, verifies it and sets up the service. A reverse proxy in front of alogeno terminates HTTPS for three hostnames: the panel, the preview and the site.

## Requirements

| | Minimum | Notes |
| :--- | :--- | :--- |
| Architecture | x86_64 or aarch64 | Static binaries, independent of the C library of the distribution |
| systemd | 254 | Needed for `DelegateSubgroup=`. Debian 13, Ubuntu 24.04, Fedora 40, Arch Linux and openSUSE Tumbleweed qualify; Debian 12, Ubuntu 22.04 and RHEL 9 do not |
| cgroups | v2 (unified hierarchy) | The default on every distribution listed above |
| User namespaces | Available to unprivileged users | On Ubuntu 23.10 and later the installer adds an AppArmor profile for bubblewrap |
| Memory | 2 GB, 4 GB recommended | By default the agent may use 40% of the memory (at most 2 GiB) and a build 50% (at most 3 GiB) |
| Disk | 5 GB free | Runtimes downloaded on first use (Node.js, Claude Code, Codex: several hundred MB together), the site with its `node_modules`, published releases, media |
| Network | Outbound HTTPS | Runtime downloads, the assistant's API, npm |

The installer installs `bubblewrap`, `git` and `ca-certificates` when missing, through `apt-get`, `dnf`, `pacman` or `zypper`.

The hostnames need DNS records pointing to the server, for example `panel.example.com`, `preview.example.com`, `example.com` and `www.example.com`. HTTPS is required: the session and preview cookies are `Secure`.

## Install

```bash
curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
```

The installer asks for the domain of the site and derives the panel (`panel.<domain>`) and preview (`preview.<domain>`) hostnames from it. Without a terminal, for example in provisioning scripts, the domain comes from `ALOGENO_DOMAIN`. Variables go after `sudo`, which does not pass the caller's environment:

```bash
curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh \
  | sudo ALOGENO_DOMAIN=example.com bash
```

| Variable | Effect |
| :--- | :--- |
| `ALOGENO_DOMAIN` | Domain of the site on a new installation |
| `ALOGENO_VERSION` | Release tag to install, for example `v0.1.0`; the latest release when unset |
| `ALOGENO_BASE_URL` | Directory or URL holding the release archives and `SHA256SUMS`, instead of GitHub |
| `ALOGENO_PREFIX` | Installs the files under this directory without root and without touching systemd; for testing the installer |

The installer, in order:

1. checks the architecture and the version of systemd;
2. downloads `alogeno-<arch>-linux.tar.gz` and `SHA256SUMS` from the release and verifies the checksum;
3. installs the missing packages (`bubblewrap`, `git`, `ca-certificates`);
4. writes the files listed below, keeping an existing configuration;
5. creates the `alogeno` system user, enables `alogeno.socket` and `alogeno.service`;
6. starts the service, waits until it listens, and shows the result of the environment checks the service ran at start;
7. prints the one-time link that creates the owner of the installation.

| Path | Content |
| :--- | :--- |
| `/usr/local/bin/alogeno` | The binary, with the panel embedded |
| `/etc/alogeno/alogeno.toml` | Configuration |
| `/etc/systemd/system/alogeno.service`, `alogeno.socket` | Service and listening socket (`127.0.0.1:4100`) |
| `/etc/sysusers.d/alogeno.conf` | The `alogeno` system user |
| `/etc/apparmor.d/alogeno-bwrap` | Only where AppArmor restricts user namespaces |
| `/var/lib/alogeno` | Data directory, created by systemd, readable by `alogeno` only |

The checksum guards against corrupted downloads. `SHA256SUMS` comes from the same release as the archive, so it does not replace trust in the GitHub repository.

## The owner

The first start prints a link valid for 24 hours that creates the owner's account. Each restart prints a new one until the owner exists, and earlier links stay valid. The link is also in the log:

```bash
journalctl -u alogeno | grep setup#
```

After the owner exists, the same command resets the owner's password:

```bash
sudo runuser -u alogeno -- alogeno --config /etc/alogeno/alogeno.toml owner-link
```

## Reverse proxy

alogeno listens on `127.0.0.1:4100` and tells the panel, the preview and the site apart by the `Host` header. The proxy forwards every hostname to the same address, keeps the original `Host`, and passes the client address in `X-Forwarded-For`. The panel uses Server-Sent Events for the chat and a WebSocket for the terminal, so the proxy must not buffer responses and must allow upgrades. Media uploads are up to 20 MiB.

### Caddy

Caddy obtains the certificates, keeps `Host`, sets `X-Forwarded-For`, streams events and upgrades WebSockets without further settings:

```caddy
panel.example.com, preview.example.com, example.com, www.example.com {
	reverse_proxy 127.0.0.1:4100
}
```

### nginx

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 80;
    listen [::]:80;
    server_name panel.example.com preview.example.com example.com www.example.com;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    server_name panel.example.com preview.example.com example.com www.example.com;

    ssl_certificate     /etc/letsencrypt/live/example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/example.com/privkey.pem;

    client_max_body_size 25m;

    location / {
        proxy_pass http://127.0.0.1:4100;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_buffering off;
        proxy_read_timeout 1h;
    }
}
```

The certificate must cover all four hostnames, for example with `certbot certonly --nginx -d example.com -d www.example.com -d panel.example.com -d preview.example.com`.

### A proxy on another machine

alogeno trusts `X-Forwarded-For` only from the addresses in `proxy.trusted`, by default `127.0.0.1` and `::1`. A proxy on another machine needs its address there, and the socket must listen on an address that machine can reach:

```toml
[proxy]
trusted = ["10.0.0.2"]
```

```bash
sudo systemctl edit alogeno.socket
```

```ini
[Socket]
ListenStream=
ListenStream=10.0.0.1:4100
```

```bash
sudo systemctl restart alogeno.socket alogeno.service
```

The port is then reachable from the network: a firewall must restrict it to the proxy.

## Configuration

`/etc/alogeno/alogeno.toml`, applied with `sudo systemctl restart alogeno`.

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `listen` | | Address to listen on when systemd does not pass a socket; ignored in the default installation, where `alogeno.socket` decides |
| `data_dir` | | Data directory; a relative path is resolved against the directory of the configuration file |
| `hosts.panel` | | Hostname of the panel |
| `hosts.preview` | | Hostname of the preview of unpublished changes |
| `hosts.site` | | Hostnames of the published site |
| `hosts.scheme` | `"https"` | How browsers reach the hostnames |
| `hosts.port` | | Port in public addresses, when not the default of the scheme |
| `proxy.trusted` | `["127.0.0.1", "::1"]` | Addresses whose client header is trusted |
| `proxy.header` | `"x-forwarded-for"` | Header that carries the client address |
| `limits.agent_memory` | 40% of memory, at most 2G | Memory ceiling of the agent, for example `"1536M"` |
| `limits.build_memory` | 50% of memory, at most 3G | Memory ceiling of a build |
| `sandbox.build_allow` | `[]` | Hosts the builds may reach on top of the built-in ones, for example `"*.githubusercontent.com"` |
| `sandbox.agent_allow` | `[]` | Hosts the agent may reach on top of the built-in ones |

Without a `[runtimes]` section alogeno downloads and manages Node.js, Claude Code and Codex in fixed versions under the data directory.

## Checking the machine

The service checks the environment at every start. Until the checks pass, the panel works but the agent and the builds stay disabled. The log of the last start shows the result:

```bash
sudo systemctl restart alogeno
journalctl -u alogeno -b | grep -E 'environment ready|FAIL|warn'
```

Only the checks run by the service are conclusive: the unit's hardening and cgroup delegation are part of what they test. `alogeno check` run in a shell is a quick diagnosis of the machine.

| Check | Requirement |
| :--- | :--- |
| `systemd` | systemd is the init system |
| `cgroup v2` | Unified cgroup hierarchy mounted |
| `delegation` | The service's cgroup is delegated, with the `memory`, `cpu` and `pids` controllers |
| `user namespaces` | Unprivileged user namespaces allowed |
| `bubblewrap` | `bwrap` found and able to start a sandbox |
| `git` | `git` found |

Run outside the service, `delegation` reports a warning, not a failure.

## Upgrade

Running the installer again upgrades to the latest release, or to `ALOGENO_VERSION`:

```bash
curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
```

The configuration and the data directory stay as they are. Units and the binary are replaced; local changes to the units belong in drop-ins (`systemctl edit`). Before applying database migrations alogeno copies `alogeno.db` to `/var/lib/alogeno/backups/`. While the service restarts, `alogeno.socket` keeps incoming connections waiting instead of refusing them.

Going back to an earlier release after a migration is not supported: restore the backup taken before the upgrade, see [backup.md](backup.md).

## Uninstall

```bash
sudo systemctl disable --now alogeno.service alogeno.socket
sudo rm /usr/local/bin/alogeno \
  /etc/systemd/system/alogeno.service /etc/systemd/system/alogeno.socket \
  /etc/sysusers.d/alogeno.conf
sudo rm -f /etc/apparmor.d/alogeno-bwrap
sudo systemctl daemon-reload
```

The configuration (`/etc/alogeno`), the data (`/var/lib/alogeno`) and the `alogeno` user remain until removed by hand:

```bash
sudo rm -r /etc/alogeno /var/lib/alogeno
sudo userdel alogeno
```

The data directory holds the site, its history and the credentials of the assistants.
