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

https://github.com/user-attachments/assets/387d6cf7-2410-4bc8-939f-85cea22c2f49

## How it works

1. **Install** with one command on a Linux server.
2. **Connect** a Claude or ChatGPT subscription.
3. **Ask** for pages, text and fixes in a chat, and watch the preview change.
4. **Publish** the changes that are ready, at the site's own domain.

## Install

```bash
curl -fsSL https://get.damasco.dev/install.sh | sudo bash
```

The installer asks for the domain of the panel, such as `panel.example.com`, and prints a link that creates the owner's account. Everything else happens in the panel: sites, their domains, the assistant. On NixOS the repository is a flake with a module, `services.damasco`: see [NixOS](docs/install.md#nixos).

## Details

- **The agent** is the official Claude Code or Codex CLI, signed in with the owner's account: one sign-in serves every site, with no credits, and the plan's usage limits apply. An Anthropic or OpenAI API key works too.
- **The instructions** for the assistant are written once in the panel, for every site and for each one, and hold in every conversation.
- **The CMS** edits pages, blocks and images through forms, without opening the chat; the fields of a page are divided into titled sections.
- **The builds** of each site are listed in the panel; the site goes back to any of them without the assistant.
- **Publishing** lists every pending change; the chosen ones go online together, the others wait.
- **The site** is a standard [Astro](https://astro.build) project in a git repository on the server, built into static files.
- **The sandbox**: the agent and the builds run inside bubblewrap, under memory and process limits, with network access through an allowlist.
- **The server**: one binary for x86_64 or aarch64, managed by systemd 254 or later (Debian 13, Ubuntu 24.04, Fedora 40 and newer), with cgroup v2, unprivileged user namespaces and 2 GB of memory. One installation serves several sites, each with its own domains, and new releases install from the panel.

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
| 2 | Websites: several sites per installation, each with its own domains and preview; chat with Claude Code or Codex, CMS, selective publishing |
| 3 | Small private apps: server functions, scheduled jobs, data and secrets, notifications |
| 4 | Public apps with payments through Stripe, up to a small online shop |

## On the server

damasco runs as the systemd service `damasco`, under its own `damasco` user.

```bash
systemctl status damasco                 # whether it runs
journalctl -u damasco -f                 # its log, with the result of the environment checks
sudo systemctl restart damasco           # after a change to the configuration
sudo damasco update                      # installs the latest release of the same major version, as the panel button does
sudo damasco owner-link                  # a new link for the owner
sudo damasco check                       # what the machine lacks
```

The configuration is `/etc/damasco/damasco.toml`, written by the installer from the domain of the panel it asked for:

```toml
listen = "127.0.0.1:4100"            # ignored: damasco.socket holds the port
data_dir = "/var/lib/damasco"        # the sites, their pictures and databases, sign-ins of the assistants

[hosts]
panel = "panel.example.com"          # the preview of each site: <slug>.preview.panel.example.com

[domains]
ask_listen = "127.0.0.1:4099"        # Caddy asks here before getting a certificate


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

The sites' own domains are not in the file: they are added in the panel, and are served at once. Every key is described in [installation](docs/install.md#configuration). Caddy in front serves the panel, the previews and those domains, asking damasco at `127.0.0.1:4099` before each certificate. With nginx, or for a second installation on the same machine, the names go into the proxy by hand: [reverse proxy](docs/install.md#reverse-proxy).

## Development

The binary is Rust (the version is pinned in `rust-toolchain.toml`) with the panel embedded, a React app built with Node.js 26. `shell.nix` provides both, plus bubblewrap and SQLite; with direnv, `.envrc` points `DAMASCO_CONFIG` at `dev/damasco.toml`.

```bash
scripts/dev                      # rebuilds the panel and restarts the server on every change
cargo test && cargo clippy       # Rust tests and lints
(cd panel && npm run check)      # panel types and lints
scripts/vm start                 # a local VM for trying install.sh (needs qemu, cloud-utils)
```

In development the panel answers on `http://panel.localhost:4100`, the preview of each site on `<slug>.preview.panel.localhost:4100` and the published site on `<slug>.localhost:4100`. Release archives come from `scripts/package`, run by the release workflow on every `v*` tag. The flake builds the same binary (`nix build`); after a change to `panel/package-lock.json`, `npmDepsHash` in `nix/package.nix` changes too, and `nix build` prints the new one.

## Contributing

Bugs, questions and ideas go to the [issues](https://github.com/x71c9/damasco/issues).

## License

[AGPL-3.0](LICENSE). The starter files copied into each website use a permissive license, so every website belongs entirely to its owner.

Logo by [Jacopo Tripodi](https://www.jacopotripodi.com/).

<sub>Lovable, Bolt, v0, Base44, Claude, Claude Code, ChatGPT and Codex are trademarks of their respective owners. damasco is an independent project, not affiliated with or endorsed by any of them.</sub>
