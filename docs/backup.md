# Backup and restore

damasco does not back itself up. Everything an installation holds lives in two places, and copying them elsewhere is the job of whoever runs the server, as with any self-hosted service:

- the configuration, `/etc/damasco/damasco.toml`;
- the data directory, `/var/lib/damasco` (`data_dir` in the configuration).

The commands below name the default installation. For a named one, such as `blog` (see [several installations](install.md#several-installations-on-one-machine)), the paths are `/etc/damasco-blog` and `/var/lib/damasco-blog`, the units `damasco@blog.service` and `damasco@blog.socket`, the user `damasco-blog`.

## The data directory

| Path | Content | In a backup |
| :--- | :--- | :--- |
| `installation.db` (with `-wal` and `-shm` while running) | SQLite database of the installation: the owner, sessions, sites, domains, activity log, assistant settings | Yes, copied safely (see below) |
| `sites/<id>/site.db` (with `-wal` and `-shm`) | SQLite database of one site: conversations, publications, preview sessions | Yes, copied safely |
| `sites/<id>/site/` | git repository of the site: code, content, schema, full history, list of its builds (`refs/damasco/builds`, `refs/damasco/failed-builds`) | Yes |
| `instructions.md` | The owner's instructions to the assistant for every site | Yes |
| `sites/<id>/agent/` | The site's sessions of the assistants, the owner's instructions for the site (`instructions.md`) and the files attached in its chat (`attachments/`) | Yes |
| `sites/<id>/media/` | Uploaded images and files, named by their SHA-256 | Yes |
| `sites/<id>/releases/` | Published versions of the site; its domains are served from here | Yes |
| `sites/<id>/nested-git/` | The `.git` of repositories found inside the site, set aside when it is saved | Optional |
| `sites/<id>/build/` | Output of the last builds | Optional, rebuilt from `site/` |
| `sites/<id>/site/node_modules/` | Packages of the site | Optional, installed again by the next build |
| `agent/` | Homes of the assistants: sign-ins, API keys, CLI session histories | Yes, encrypted, or left out (see below) |
| `runtimes/` | Node.js, Claude Code and Codex in the versions damasco pins | No, downloaded again when missing |
| `backups/`, `sites/<id>/backups/` | Copies of the databases taken before migrations | Optional |

`<id>` is the identifier of the site, which never changes; `installation.db` maps each one to its slug and name. The `backups/` directories protect against a failed upgrade on the same machine. They are not a backup: they sit on the same disk and hold only the databases.

## SQLite is not copied with `cp`

damasco writes its databases in WAL mode. Copying a file while the service runs can produce a database that is missing recent changes or does not open. There are two safe ways.

### Stopping the service

The simplest and fully consistent copy. damasco is unavailable for the time of the copy, usually seconds.

`damasco.socket` starts the service again at the next request, so both units stop:

```bash
sudo systemctl stop damasco.socket damasco.service
sudo tar -C / -czf /root/damasco-$(date +%F).tar.gz \
  --exclude=var/lib/damasco/runtimes \
  --exclude='var/lib/damasco/sites/*/site/node_modules' \
  etc/damasco var/lib/damasco
sudo systemctl start damasco.socket damasco.service
```

On stop, a turn of the agent in progress gets a few seconds to save what it wrote; a copy taken while the agent is idle loses nothing.

### Without stopping

`sqlite3` (package `sqlite3` or `sqlite`) copies an open database consistently, one database at a time: `installation.db`, then the `site.db` of each site, each to the same relative path under the backup directory. The rest of the data directory is copied next to them:

```bash
sudo install -d -o damasco -g damasco -m 0700 /var/backups/damasco
sudo runuser -u damasco -- bash -c '
  cd /var/lib/damasco
  for db in installation.db sites/*/site.db; do
    mkdir -p "/var/backups/damasco/$(dirname "$db")"
    sqlite3 "$db" ".backup /var/backups/damasco/$db"
  done'
sudo tar -C / -czf /var/backups/damasco/files.tar.gz \
  --exclude='var/lib/damasco/installation.db*' \
  --exclude='var/lib/damasco/sites/*/site.db*' \
  --exclude=var/lib/damasco/runtimes \
  --exclude='var/lib/damasco/sites/*/site/node_modules' \
  etc/damasco var/lib/damasco
```

The copies run as `damasco`, so that no file of a database ends up owned by root. The databases and the other files are copied moments apart: a copy taken while the agent is writing a site may catch its working tree halfway through a change, and a site created or deleted during the copy may be in one part and not in the other. The history in `site/.git` stays intact; a copy taken while no one works in the panel avoids both cases.

## Credentials

`agent/` holds the sign-ins of the assistants (Claude, ChatGPT) and any API key saved in the panel, shared by every site of the installation. Anyone holding a copy can use those accounts. Two options:

- encrypt the copy before it leaves the machine, for example with [age](https://github.com/FiloSottile/age) or GnuPG:

  ```bash
  age -r <recipient> -o damasco-2026-11-21.tar.gz.age damasco-2026-11-21.tar.gz
  ```

  Backup tools such as restic or Borg encrypt by default;
- leave `agent/` out of the copy (`--exclude=var/lib/damasco/agent`) and connect the assistant again from the panel after a restore. Past conversations stay listed; continuing one of them in the CLI may not be possible.

`installation.db` holds the hash of the owner's password and hashed session tokens, never passwords or tokens in clear. Encrypting the whole copy is still the safer default.

## Off the machine

A backup on the same disk does not survive the loss of the server. The archive goes to another machine or to object storage, on a schedule: a systemd timer or cron running one of the procedures above, followed by the upload.

## Restore

A restore needs the same release of damasco that made the copy, or a newer one. A newer release migrates the databases at start, after copying each one to its `backups/`. An older release refuses a database migrated by a newer one.

1. On the new machine, install damasco with the same panel domain (see [install.md](install.md)) and stop it:

   ```bash
   curl -fsSL https://github.com/x71c9/damasco/releases/latest/download/install.sh \
     | sudo DAMASCO_PANEL=panel.example.com bash
   sudo systemctl stop damasco.socket damasco.service
   ```

2. Replace the configuration and the data directory with the copy:

   ```bash
   sudo rm -r /var/lib/damasco /etc/damasco
   sudo tar -C / -xzf damasco-2026-11-21.tar.gz
   ```

   With the copy taken without stopping, copied back to `/var/backups/damasco`, the databases go back in their places:

   ```bash
   sudo tar -C / -xzf /var/backups/damasco/files.tar.gz
   sudo bash -c '
     cd /var/backups/damasco
     for db in installation.db sites/*/site.db; do
       install -m 0600 "$db" "/var/lib/damasco/$db"
     done'
   ```

3. Give the files to the `damasco` user, whose numeric id can differ between machines:

   ```bash
   sudo chown -R damasco:damasco /var/lib/damasco
   sudo chmod 0700 /var/lib/damasco
   ```

4. Start and check:

   ```bash
   sudo systemctl start damasco.socket damasco.service
   journalctl -u damasco -b --no-pager
   ```

   The log reports the database of the installation and its schema version, the result of the environment checks, and the address it listens on. Missing runtimes are downloaded again, and the next build of each site installs its packages again.

5. Point the DNS to the new machine: the panel, the previews' wildcard `*.preview.<panel>`, and the domains of every site, whose records the panel lists with the new addresses (see [Sites and domains](install.md#sites-and-domains)). With [Caddy asking damasco](install.md#caddy-with-domains-from-the-panel), the certificates are obtained again on the first visit of each name; with [names by hand](install.md#names-by-hand), the proxy of the new machine needs every name the old one had. Sessions stay valid, so a signed-in owner remains signed in. Without `agent/` in the copy, the owner connects the assistant again in Settings › Assistant.

## Testing the procedure

A backup that has never been restored is not known to work. Restoring a copy on a second machine, or in a virtual machine, and comparing each site, its pending changes and its published version with the original verifies both the copy and the procedure.
