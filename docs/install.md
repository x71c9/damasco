# Installation

damasco is a single binary that runs as a systemd service on a Linux server. The installer downloads a release, verifies it and sets up the service. A reverse proxy in front of damasco terminates HTTPS for three hostnames: the panel, the preview and the site.

## Requirements

| | Minimum | Notes |
| :--- | :--- | :--- |
| Architecture | x86_64 or aarch64 | Static binaries, independent of the C library of the distribution |
| systemd | 254 | Needed for `DelegateSubgroup=`. Debian 13, Ubuntu 24.04, Fedora 40, Arch Linux, openSUSE Tumbleweed and Raspberry Pi OS based on Debian 13 qualify; Debian 12, Raspberry Pi OS based on Debian 12, Ubuntu 22.04 and RHEL 9 do not |
| cgroups | v2 (unified hierarchy) | The default on every distribution listed above |
| User namespaces | Available to unprivileged users | On Ubuntu 23.10 and later the installer adds an AppArmor profile for bubblewrap |
| Memory | 2 GB, 4 GB recommended | By default the agent may use 40% of the memory (at most 2 GiB) and a build 50% (at most 3 GiB) |
| Disk | 5 GB free | Runtimes downloaded on first use (Node.js, Claude Code, Codex: several hundred MB together), the site with its `node_modules`, published releases, media |
| Network | Outbound HTTPS | Runtime downloads, the assistant's API, npm |

The installer installs `bubblewrap`, `git`, `ca-certificates` and `libatomic` (needed by the Node.js that damasco downloads) when missing, through `apt-get`, `dnf`, `pacman` or `zypper`.

Installation is tested before each release on Ubuntu 24.04 (x86_64 and aarch64), and by hand on Debian 13 and Fedora 45 with SELinux enforcing. The aarch64 builds of damasco, Node.js, Claude Code and Codex are compatible with kernels using 16K pages, such as the one of the Raspberry Pi 5: their segments are aligned to 64K and their allocators do not assume 4K pages.

The hostnames need DNS records pointing to the server, for example `panel.example.com`, `preview.example.com`, `example.com` and `www.example.com`. HTTPS is required: the session and preview cookies are `Secure`.

## Install

```bash
curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh | sudo bash
```

The installer asks for the domain of the panel, for example `panel.example.com`; the preview of each site answers on `<slug>.preview.<panel>`, such as `blog.preview.panel.example.com`. Without a terminal, for example in provisioning scripts, the domain comes from `DAMASCO_PANEL`. Variables go after `sudo`, which does not pass the caller's environment:

```bash
curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh \
  | sudo DAMASCO_PANEL=panel.example.com bash
```

| Variable | Effect |
| :--- | :--- |
| `DAMASCO_PANEL` | Domain of the panel on a new installation |
| `DAMASCO_INSTANCE` | Name of an installation besides the default one, see [Several installations on one machine](#several-installations-on-one-machine) |
| `DAMASCO_PORT` | Local port of a new installation: 4100 for the default one, the first free one from 4101 for the others when unset |
| `DAMASCO_ASK_PORT` | How a new installation brings its names to the reverse proxy. A port: Caddy asks damasco on it before getting a certificate, so domains added in the panel work at once; the default installation uses 4099 when unset. `none`: the names go into the proxy by hand, the default for named installations and the choice for nginx. Only one installation per machine asks on a port |
| `DAMASCO_UPGRADE` | `1`: only replaces the binary and the units and restarts every installation, never creates one (what `damasco update` does) |
| `DAMASCO_VERSION` | Release tag to install, for example `v0.1.0`; the latest release when unset |
| `DAMASCO_BASE_URL` | Directory or URL holding the release archives and `SHA256SUMS`, instead of GitHub |
| `DAMASCO_PREFIX` | Installs the files under this directory without root and without touching systemd; for testing the installer |

The installer, in order:

1. checks the architecture and the version of systemd;
2. downloads `damasco-<arch>-linux.tar.gz` and `SHA256SUMS` from the release and verifies the checksum;
3. installs the missing packages (`bubblewrap`, `git`, `ca-certificates`, `libatomic`);
4. writes the files listed below, keeping an existing configuration;
5. creates the `damasco` system user, enables `damasco.socket` and `damasco.service`;
6. starts the service, waits until it listens, and shows the result of the environment checks the service ran at start;
7. prints the one-time link that creates the owner of the installation.

| Path | Content |
| :--- | :--- |
| `/usr/local/bin/damasco` | The binary, with the panel embedded |
| `/etc/damasco/damasco.toml` | Configuration |
| `/etc/systemd/system/damasco.service`, `damasco.socket` | Service and listening socket (`127.0.0.1:4100`) |
| `/etc/systemd/system/damasco-update.service`, `damasco-update.path` | Updates asked from the panel, run as root |
| `/etc/sysusers.d/damasco.conf` | The `damasco` system user |
| `/etc/apparmor.d/damasco-bwrap` | Only where AppArmor restricts user namespaces |
| `/var/lib/damasco` | Data directory, created by systemd, readable by `damasco` only |

The checksum guards against corrupted downloads. `SHA256SUMS` comes from the same release as the archive, so it does not replace trust in the GitHub repository.

## Several installations on one machine

One installation serves several sites from one panel. A machine holds several installations, each with its own panel, sites, data, owner, assistant sign-in, system user and port, next to the default one or without it. `DAMASCO_INSTANCE` names a new one; `router` is reserved:

```bash
curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh \
  | sudo DAMASCO_INSTANCE=blog DAMASCO_PANEL=panel.blog.example.com bash
```

| | Default installation | Installation `blog` |
| :--- | :--- | :--- |
| Service and socket | `damasco.service`, `damasco.socket` | `damasco@blog.service`, `damasco@blog.socket` |
| Port | 4100 | the first free one from 4101, or `DAMASCO_PORT`; in `damasco@blog.socket.d/listen.conf` |
| System user | `damasco` | `damasco-blog` |
| Configuration | `/etc/damasco/damasco.toml` | `/etc/damasco-blog/damasco.toml` |
| Data | `/var/lib/damasco` | `/var/lib/damasco-blog` |
| Updates from the panel | `damasco-update.path` | `damasco-update@blog.path` |
| Owner link | `sudo damasco owner-link` | `sudo damasco --instance blog owner-link` |

Names use lower-case letters, digits and `-`, up to 32 characters. The reverse proxy sends the hostnames of each installation to its port. The binary, the AppArmor profile and the units are shared: installing or upgrading any installation restarts all of them on the new binary. Each installation downloads its own runtimes into its data directory and has its own memory limits, computed from the memory of the whole machine: on a small machine `limits.agent_memory` and `limits.build_memory` keep several installations within it.

## NixOS

On NixOS the installer does not apply: units and users belong in the system configuration. The repository is a flake with the package and a NixOS module:

```nix
{
  inputs.damasco.url = "github:x71c9/damasco/v2.0.0";

  outputs = { nixpkgs, damasco, ... }: {
    nixosConfigurations.server = nixpkgs.lib.nixosSystem {
      modules = [
        damasco.nixosModules.default
        {
          nixpkgs.config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "claude-code";
          services.damasco = {
            enable = true;
            panelDomain = "panel.example.com";
            askPort = 4099;
            caddy.enable = true;
            instances.blog = { panelDomain = "panel.blog.example.com"; port = 4101; };
          };
        }
      ];
    };
  };
}
```

| Option | Default | Meaning |
| :--- | :--- | :--- |
| `services.damasco.enable` | `false` | The default installation |
| `services.damasco.panelDomain` | | Domain of the panel; the previews answer on `<slug>.preview.<panelDomain>`. |
| `services.damasco.port` | `4100` | Local port the reverse proxy forwards to |
| `services.damasco.askPort` | `null` | Local port where Caddy asks whether a name is served, before getting its certificate; domains added in the panel then work without touching the proxy. One installation per machine at most |
| `services.damasco.caddy.enable` | `false` | Caddy in front of the installation with `askPort`: one block serves its panel, previews and domains, with on-demand certificates. Other installations go into `services.caddy.virtualHosts` by hand |
| `services.damasco.settings` | `{ }` | Further keys of `damasco.toml`, such as `limits` or `sandbox` |
| `services.damasco.instances.<name>` | `{ }` | More installations, each with `panelDomain`, `port` (required), `askPort` and `settings` |
| `services.damasco.nodejs`, `claudeCode` | `nodejs_26`, `claude-code` | The runtimes, from nixpkgs |
| `services.damasco.package` | the flake's | The damasco package |

The module creates the same names and paths as the installer: `damasco.service` with the user `damasco`, `/etc/damasco/damasco.toml` and `/var/lib/damasco`; for `instances.blog`, `damasco-blog.service`, `/etc/damasco-blog/damasco.toml` and `/var/lib/damasco-blog`. The rest of this guide applies as it is, apart from these differences:

- **Runtimes.** The Node.js and Claude Code builds damasco downloads expect a dynamic loader NixOS does not have, so they come from nixpkgs, in the versions nixpkgs carries. Claude Code is unfree, hence the `allowUnfreePredicate` above. Codex, a static binary, is still downloaded by damasco.
- **Upgrades.** The version follows the flake input: `nix flake update damasco`, then `nixos-rebuild switch`. The panel shows a newer release with that command instead of a button, and `damasco update` refuses to run.
- **Reverse proxy.** The examples for Caddy and nginx translate to `services.caddy.virtualHosts` or `services.nginx.virtualHosts`, with `proxyPass` and WebSockets enabled.

## The owner

The first start prints a link valid for 24 hours that creates the owner's account. Each restart prints a new one until the owner exists, and earlier links stay valid. The link is also in the log:

```bash
journalctl -u damasco | grep setup#
```

A new link, which resets the owner's password once the owner exists, comes from the binary. Run as root, it acts as the installation's own user, so the files it writes stay readable by the service; `--instance blog` picks a named installation:

```bash
sudo damasco owner-link
sudo damasco --instance blog owner-link
```

## Reverse proxy

damasco listens on `127.0.0.1:4100` and tells the panel, the preview and the site apart by the `Host` header. The proxy forwards every hostname to the same address, keeps the original `Host`, and passes the client address in `X-Forwarded-For`. The panel uses Server-Sent Events for the chat and a WebSocket for the terminal, so the proxy must not buffer responses and must allow upgrades. Media uploads are up to 20 MiB.

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

damasco trusts `X-Forwarded-For` only from the addresses in `proxy.trusted`, by default `127.0.0.1` and `::1`. A proxy on another machine needs its address there, and the socket must listen on an address that machine can reach:

```toml
[proxy]
trusted = ["10.0.0.2"]
```

```bash
sudo systemctl edit damasco.socket
```

```ini
[Socket]
ListenStream=
ListenStream=10.0.0.1:4100
```

```bash
sudo systemctl restart damasco.socket damasco.service
```

The port is then reachable from the network: a firewall must restrict it to the proxy.

## Configuration

`/etc/damasco/damasco.toml`, applied with `sudo systemctl restart damasco`.

| Key | Default | Meaning |
| :--- | :--- | :--- |
| `listen` | | Address to listen on when systemd does not pass a socket; ignored in the default installation, where `damasco.socket` decides |
| `data_dir` | | Data directory; a relative path is resolved against the directory of the configuration file |
| `hosts.panel` | | Hostname of the panel; the preview of each site is `<slug>.preview.<panel>`, and the sites' own domains are added in the panel |
| `hosts.scheme` | `"https"` | How browsers reach the hostnames |
| `hosts.port` | | Port in public addresses, when not the default of the scheme |
| `domains.ask_listen` | | Loopback address where Caddy asks whether a name is served (`GET /ask?domain=<name>`: 200 or 403), before getting its certificate. Unset: names go into the proxy by hand |
| `proxy.trusted` | `["127.0.0.1", "::1"]` | Addresses whose client header is trusted |
| `proxy.header` | `"x-forwarded-for"` | Header that carries the client address |
| `limits.agent_memory` | 40% of memory, at most 2G | Memory ceiling of the agent, for example `"1536M"` |
| `limits.build_memory` | 50% of memory, at most 3G | Memory ceiling of a build |
| `sandbox.build_allow` | `[]` | Hosts the builds may reach on top of the built-in ones, for example `"*.githubusercontent.com"` |
| `sandbox.agent_allow` | `[]` | Hosts the agent may reach on top of the built-in ones |

Without a `[runtimes]` section damasco downloads and manages Node.js, Claude Code and Codex in fixed versions under the data directory.

## Checking the machine

The service checks the environment at every start. Until the checks pass, the panel works but the agent and the builds stay disabled. The log of the last start shows the result:

```bash
sudo systemctl restart damasco
journalctl -u damasco -b | grep -E 'environment ready|FAIL|warn'
```

Only the checks run by the service are conclusive: the unit's hardening and cgroup delegation are part of what they test. `sudo damasco check` is a quick diagnosis of the machine, run as the installation's user.

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

Three ways lead to the same result:

- **From the panel.** Settings shows the version at the foot of the menu and, to the owner and administrators, a button when a newer release exists. The service runs unprivileged and cannot replace its own binary: the button leaves an empty request file in the data directory, and `damasco-update.path` starts `damasco-update.service`, which runs `damasco update` as root. The panel reloads when the new version answers.
- **With the binary,** as root; `--version` picks a release:

  ```bash
  sudo damasco update
  sudo damasco update --version v0.1.2
  ```

- **With the installer,** run again; `DAMASCO_VERSION` picks a release:

  ```bash
  curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh | sudo bash
  ```

`damasco update` downloads the installer of the release from GitHub and runs it with `DAMASCO_UPGRADE=1`, so all three install the same files. The binary is shared by every installation on the machine, so an upgrade restarts all of them, whichever panel asked for it.

Upgrades stay within the major version. The panel and `damasco update` offer the newest release of the installed major version, even when GitHub marks a newer major version as the latest release, and `damasco update --version` refuses a release of another major version. A new major version may change the configuration and the data directory: its release notes say what it needs.

The configuration and the data directory stay as they are. Units and the binary are replaced; local changes to the units belong in drop-ins (`systemctl edit`). Before applying database migrations damasco copies the database to a `backups/` directory next to it: `installation.db` to `/var/lib/damasco/backups/`, and the `site.db` of each site to the `backups/` of its folder. While the service restarts, `damasco.socket` keeps incoming connections waiting instead of refusing them.

Going back to an earlier release after a migration is not supported: restore the backup taken before the upgrade, see [backup.md](backup.md).

## Uninstall

```bash
sudo systemctl disable --now damasco.service damasco.socket damasco-update.path
sudo rm /usr/local/bin/damasco \
  /etc/systemd/system/damasco.service /etc/systemd/system/damasco.socket \
  /etc/systemd/system/damasco-update.service /etc/systemd/system/damasco-update.path \
  /etc/sysusers.d/damasco.conf
sudo rm -f /etc/apparmor.d/damasco-bwrap
sudo systemctl daemon-reload
```

The configuration (`/etc/damasco`), the data (`/var/lib/damasco`) and the `damasco` user remain until removed by hand:

```bash
sudo rm -r /etc/damasco /var/lib/damasco
sudo userdel damasco
```

The data directory holds the site, its history and the credentials of the assistants.

A named installation goes on its own, leaving the others running:

```bash
sudo systemctl disable --now damasco@blog.service damasco@blog.socket damasco-update@blog.path
sudo rm -r /etc/systemd/system/damasco@blog.socket.d /etc/sysusers.d/damasco-blog.conf
sudo systemctl daemon-reload
sudo rm -r /etc/damasco-blog /var/lib/damasco-blog
sudo userdel damasco-blog
```

Removing the binary and the shared units, as above, also needs `damasco@.service`, `damasco@.socket` and `damasco-update@.path` in `/etc/systemd/system`, once no installation is left.
