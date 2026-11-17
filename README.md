<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/axioma-logo-white.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/axioma-logo-black.svg">
    <img alt="axioma" src="docs/assets/axioma-logo-black.svg" width="280">
  </picture>
</p>

<h2 align="center">Lovable, but open source and with your existing AI subscription</h2>

<p align="center">
  Describe a website in a chat. An AI agent builds it, a real CMS keeps it editable, one button publishes it.
</p>

<p align="center">
  <b>Runs on an existing AI subscription.</b><br>
  No credits, no per-message billing: the agent logs in with the Claude or ChatGPT account already paid for.
</p>

<p align="center">
  <img alt="Status: early development" src="https://img.shields.io/badge/status-early%20development-0E0F13?labelColor=0E0F13&color=3A3D46">
  <img alt="Platform: Linux x86_64 and aarch64" src="https://img.shields.io/badge/runs%20on-any%20Linux%20server-0E0F13?labelColor=0E0F13&color=3A3D46">
  <img alt="AI: existing Claude or ChatGPT subscription" src="https://img.shields.io/badge/AI-existing%20Claude%20or%20ChatGPT%20subscription-0E0F13?labelColor=0E0F13&color=FF2E8C">
</p>

<p align="center">
  <a href="#why-axioma">Why axioma</a> ·
  <a href="#bring-the-ai-subscription-already-in-use">AI subscription</a> ·
  <a href="#install">Install</a> ·
  <a href="#how-it-works">How it works</a> ·
  <a href="#features">Features</a> ·
  <a href="#roadmap">Roadmap</a>
</p>

> [!NOTE]
> axioma is in early development and not usable yet. Watch the repository to be notified of the first release.

## Why axioma

AI app builders made it possible to get a website from a sentence. They also put the site, the code and the AI bill inside someone else's cloud. axioma keeps the experience and moves everything else home.

| | Lovable, Bolt, v0, Base44 | axioma |
| :--- | :--- | :--- |
| **Where the site lives** | The vendor's cloud | Any Linux server: a VPS, a home server, a Raspberry Pi |
| **AI costs** | Credits on a monthly plan | No extra credits: the agent runs on the Claude or ChatGPT subscription already paid for |
| **The code** | Inside the platform | A standard [Astro](https://astro.build) project in a git repository on the server |
| **Editing without AI** | Through prompts | A built-in CMS for text, pages and images, for people who never open the chat |
| **License** | Proprietary | Open source, [AGPL-3.0](LICENSE) |

## Bring the AI subscription already in use

Hosted builders resell AI: every prompt spends credits, and larger sites need larger plans. axioma resells nothing. The agent is an official vendor CLI, logged in with the site owner's own account, so building and maintaining the site is covered by the subscription already paid for:

- **[Claude Code](https://claude.com/claude-code)** with a Claude Pro or Max subscription;
- **[Codex](https://developers.openai.com/codex)** with a ChatGPT subscription.

One assistant is in use at a time; switching keeps the site, its history and the other account's sign-in.

- **No metered billing**: the plan's usage limits apply. An API key of Anthropic or OpenAI works too, for those who prefer paying by use.
- **The same agent developers use every day**, with its own modes and models, not a reduced model behind a credit counter.
- **Usage in the panel**: the tokens of each answer, and the price when paying by API key.

## Install

One command on any Linux server with systemd:

```bash
curl -fsSL https://github.com/x71c9/axioma/releases/latest/download/install.sh | sudo bash
```

The script detects the architecture, downloads the release, verifies its checksum, installs bubblewrap if missing, and sets up the system user and the systemd service. Requirements: systemd 254 or later, cgroup v2 and unprivileged user namespaces; `axioma check` reports what is missing. Available from the first release.

## How it works

1. **Install** with a single command on a Linux server.
2. **Connect** a Claude or ChatGPT account. The agent is the official Claude Code or Codex CLI, as capable as the one developers use every day.
3. **Describe** the site in the chat. The agent writes it, and a live preview updates next to the conversation.
4. **Edit** text and images in the CMS, with or without the agent. Invite editors who only ever see the content.
5. **Publish** when ready, choosing exactly which changes go online. Nothing reaches the public site before that.

## Features

Planned for the first release:

- **Chat and live preview side by side**, on desktop and on the phone.
- **A real CMS**: pages, blocks, media, with forms generated from the content schema.
- **Selective publishing**: every pending change listed with its author, included or left out one by one.
- **People and roles**: owner, admins and editors, each with their own login, plus an activity log.
- **Sandboxed agent**: the agent and the builds run inside bubblewrap, with network access limited to an allowlist.
- **One binary, any Linux**: x86_64 and aarch64, managed by systemd.

## Roadmap

| Stage | Scope |
| :--- | :--- |
| **v1** | Websites: chat with Claude Code or Codex, CMS, preview, selective publishing, roles |
| **v2** | Small private apps: server functions, scheduled jobs, data and secrets, notifications |
| **v3** | Public apps with payments through Stripe, up to a small online shop |

## License

axioma is released under the [GNU Affero General Public License v3.0](LICENSE). Starter files that axioma copies into each website are released under a permissive license, so the websites themselves belong entirely to their owners.

## Credits

Logo by [Jacopo Tripodi](https://www.jacopotripodi.com/).

---

<sub>Lovable, Bolt, v0, Base44, Claude, Claude Code, ChatGPT and Codex are trademarks of their respective owners. axioma is an independent project, not affiliated with or endorsed by any of them.</sub>
