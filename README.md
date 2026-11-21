<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/damasco-logo-bianco.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/damasco-logo-nero.svg">
    <img alt="damasco" src="docs/assets/damasco-logo-nero.svg" width="220">
  </picture>
</p>

<h2 align="center">Self-hosted website builder that works with an existing AI subscription</h2>

<p align="center">
  <img alt="AI: existing Claude or ChatGPT subscription" src="https://img.shields.io/badge/AI-existing%20Claude%20or%20ChatGPT%20subscription-FF2E8C?labelColor=1E1912">
  <img alt="Runs on any Linux server" src="https://img.shields.io/badge/runs%20on-any%20Linux%20server-3D352B?labelColor=1E1912">
  <img alt="Latest release" src="https://img.shields.io/github/v/release/x71c9/damasco?label=release&color=3D352B&labelColor=1E1912">
</p>

<p align="center">
  Claude or ChatGPT build and change the site through a chat.<br>
  It runs on any Linux server, and its CMS keeps the site editable without the AI.
</p>

## How it works

1. **Install** with one command on a Linux server.
2. **Connect** a Claude or ChatGPT subscription.
3. **Ask** for pages, text and fixes in a chat, and watch the preview change.
4. **Publish** the changes that are ready.

## Install

```bash
curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh | sudo bash
```

The installer prints a link that creates the owner's account. Everything else happens in the panel. On NixOS the repository is a flake with a module, `services.damasco`: see [NixOS](docs/install.md#nixos).

## Details

- **The agent** is the official Claude Code or Codex CLI, signed in with the site owner's account: no credits, the plan's usage limits apply. An Anthropic or OpenAI API key works too.
- **The CMS** edits pages, blocks and images through forms, for editors who never open the chat. Owner, administrators and editors each have their own login, and an activity log records who did what.
- **Publishing** lists every pending change with its author; the chosen ones go online together, the others wait.
- **The site** is a standard [Astro](https://astro.build) project in a git repository on the server, built into static files.
- **The sandbox**: the agent and the builds run inside bubblewrap, under memory and process limits, with network access through an allowlist.
- **The server**: one binary for x86_64 or aarch64, managed by systemd 254 or later (Debian 13, Ubuntu 24.04, Fedora 40 and newer), with cgroup v2, unprivileged user namespaces and 2 GB of memory. One machine can host several sites, and new releases install from the panel.

Documentation: [installation](docs/install.md), with reverse proxy, configuration, several sites and upgrades; [backup and restore](docs/backup.md).

## Compared with hosted builders

| | Lovable, Bolt, v0, Base44 | damasco |
| :--- | :--- | :--- |
| Where the site lives | The vendor's cloud | Any Linux server: a VPS, a home server, a Raspberry Pi |
| AI costs | Credits on a monthly plan | The Claude or ChatGPT subscription already paid for |
| The code | Inside the platform | An Astro project in a git repository on the server |
| Editing without AI | Through prompts | A CMS for text, pages and images |
| License | Proprietary | Open source, AGPL-3.0 |

## Roadmap

| Version | Scope |
| :--- | :--- |
| 1 | Websites: chat with Claude Code or Codex, CMS, preview, selective publishing, roles |
| 2 | Small private apps: server functions, scheduled jobs, data and secrets, notifications |
| 3 | Public apps with payments through Stripe, up to a small online shop |

## On the server

damasco runs as the systemd service `damasco`, under its own `damasco` user.

```bash
systemctl status damasco                 # whether it runs
journalctl -u damasco -f                 # its log, with the result of the environment checks
sudo systemctl restart damasco           # after a change to the configuration
sudo damasco update                      # installs the latest release of the same major version, as the panel button does
sudo runuser -u damasco -- damasco --config /etc/damasco/damasco.toml owner-link   # a new link for the owner
sudo runuser -u damasco -- damasco --config /etc/damasco/damasco.toml check        # what the machine lacks
```

The configuration is `/etc/damasco/damasco.toml`, written by the installer from the domain it asked for:

```toml
listen = "127.0.0.1:4100"            # ignored: damasco.socket holds the port
data_dir = "/var/lib/damasco"        # site, media, database, sign-ins of the assistants

[hosts]
panel = "panel.example.com"
preview = "preview.example.com"
site = ["example.com", "www.example.com"]

# Optional
[proxy]
trusted = ["127.0.0.1", "::1"]       # reverse proxies whose X-Forwarded-For is trusted

[limits]
agent_memory = "2G"                  # default: 40% of the memory, at most 2G
build_memory = "3G"                  # default: 50% of the memory, at most 3G

[sandbox]
agent_allow = ["github.com"]         # hosts the agent may reach besides the built-in ones
build_allow = []                     # the same for the builds
```

Every key is described in [installation](docs/install.md#configuration). The reverse proxy in front sends the three hostnames to `127.0.0.1:4100`; examples for Caddy and nginx are in [reverse proxy](docs/install.md#reverse-proxy).

## Development

The binary is Rust (the version is pinned in `rust-toolchain.toml`) with the panel embedded, a React app built with Node.js 26. `shell.nix` provides both, plus bubblewrap and SQLite; with direnv, `.envrc` points `DAMASCO_CONFIG` at `dev/damasco.toml`.

```bash
scripts/dev                      # rebuilds the panel and restarts the server on every change
cargo test && cargo clippy       # Rust tests and lints
(cd panel && npm run check)      # panel types and lints
scripts/vm start                 # a local VM for trying install.sh (needs qemu, cloud-utils)
```

In development the panel answers on `http://panel.localhost:4100`, the preview on `preview.localhost` and the site on `site.localhost`. Release archives come from `scripts/package`, run by the release workflow on every `v*` tag. The flake builds the same binary (`nix build`); after a change to `panel/package-lock.json`, `npmDepsHash` in `nix/package.nix` changes too, and `nix build` prints the new one.

## Contributing

Bugs, questions and ideas go to the [issues](https://github.com/x71c9/damasco/issues).

## License

[AGPL-3.0](LICENSE). The starter files copied into each website use a permissive license, so every website belongs entirely to its owner.

Logo by [Jacopo Tripodi](https://www.jacopotripodi.com/).

<sub>Lovable, Bolt, v0, Base44, Claude, Claude Code, ChatGPT and Codex are trademarks of their respective owners. damasco is an independent project, not affiliated with or endorsed by any of them.</sub>
