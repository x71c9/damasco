# Backup and restore

alogeno does not back itself up. Everything an installation holds lives in two places, and copying them elsewhere is the job of whoever runs the server, as with any self-hosted service:

- the configuration, `/etc/alogeno/alogeno.toml`;
- the data directory, `/var/lib/alogeno` (`data_dir` in the configuration).

The commands below name the default installation. For a named one, such as `blog` (see [several installations](install.md#several-installations-on-one-machine)), the paths are `/etc/alogeno-blog` and `/var/lib/alogeno-blog`, the units `alogeno@blog.service` and `alogeno@blog.socket`, the user `alogeno-blog`.

## The data directory

| Path | Content | In a backup |
| :--- | :--- | :--- |
| `alogeno.db` (with `-wal` and `-shm` while running) | SQLite database: people, sessions, roles, activity log, conversations, settings | Yes, copied safely (see below) |
| `site/` | git repository of the site: code, content, schema, full history | Yes |
| `media/` | Uploaded images and files, named by their SHA-256 | Yes |
| `releases/` | Published versions of the site; the public site is served from here | Yes |
| `build/` | Output of the last preview build | Optional, rebuilt from `site/` |
| `agent/` | Homes of the assistants: sign-ins, API keys, CLI session histories | Yes, encrypted, or left out (see below) |
| `runtimes/` | Node.js, Claude Code and Codex in the versions alogeno pins | No, downloaded again when missing |
| `backups/` | Copies of `alogeno.db` taken before database migrations | Optional |

`backups/` protects against a failed upgrade on the same machine. It is not a backup: it sits on the same disk and holds only the database.

## SQLite is not copied with `cp`

alogeno writes to `alogeno.db` in WAL mode. Copying the file while the service runs can produce a database that is missing recent changes or does not open. There are two safe ways.

### Stopping the service

The simplest and fully consistent copy. alogeno is unavailable for the time of the copy, usually seconds.

`alogeno.socket` starts the service again at the next request, so both units stop:

```bash
sudo systemctl stop alogeno.socket alogeno.service
sudo tar -C / -czf /root/alogeno-$(date +%F).tar.gz \
  --exclude=var/lib/alogeno/runtimes \
  etc/alogeno var/lib/alogeno
sudo systemctl start alogeno.socket alogeno.service
```

On stop, a turn of the agent in progress gets a few seconds to save what it wrote; a copy taken while the agent is idle loses nothing.

### Without stopping

`sqlite3` (package `sqlite3` or `sqlite`) copies the open database consistently. The rest of the directory is copied next to it:

```bash
sudo install -d -o alogeno -g alogeno -m 0700 /var/backups/alogeno
sudo runuser -u alogeno -- sqlite3 /var/lib/alogeno/alogeno.db \
  ".backup /var/backups/alogeno/alogeno.db"
sudo tar -C / -czf /var/backups/alogeno/files.tar.gz \
  --exclude=var/lib/alogeno/alogeno.db \
  --exclude=var/lib/alogeno/alogeno.db-wal \
  --exclude=var/lib/alogeno/alogeno.db-shm \
  --exclude=var/lib/alogeno/runtimes \
  etc/alogeno var/lib/alogeno
```

The database copy runs as `alogeno`, so that no file of the database ends up owned by root. The database and the other files are copied a moment apart: a copy taken while the agent is writing the site may catch the working tree halfway through a change. The history in `site/.git` stays intact; a copy taken while the agent is idle avoids the case.

## Credentials

`agent/` holds the sign-ins of the assistants (Claude, ChatGPT) and any API key saved in the panel. Anyone holding a copy can use those accounts. Two options:

- encrypt the copy before it leaves the machine, for example with [age](https://github.com/FiloSottile/age) or GnuPG:

  ```bash
  age -r <recipient> -o alogeno-2026-11-18.tar.gz.age alogeno-2026-11-18.tar.gz
  ```

  Backup tools such as restic or Borg encrypt by default;
- leave `agent/` out of the copy (`--exclude=var/lib/alogeno/agent`) and connect the assistant again from the panel after a restore. Past conversations stay listed; continuing one of them in the CLI may not be possible.

`alogeno.db` holds password hashes and hashed session tokens, never passwords or tokens in clear. Encrypting the whole copy is still the safer default.

## Off the machine

A backup on the same disk does not survive the loss of the server. The archive goes to another machine or to object storage, on a schedule: a systemd timer or cron running one of the procedures above, followed by the upload.

## Restore

A restore needs the same release of alogeno that made the copy, or a newer one. A newer release migrates the database at start, after copying it to `backups/`. An older release refuses a database migrated by a newer one.

1. On the new machine, install alogeno with the same panel domain (see [install.md](install.md)) and stop it:

   ```bash
   curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh \
     | sudo ALOGENO_PANEL=panel.example.com bash
   sudo systemctl stop alogeno.socket alogeno.service
   ```

2. Replace the configuration and the data directory with the copy:

   ```bash
   sudo rm -r /var/lib/alogeno /etc/alogeno
   sudo tar -C / -xzf alogeno-2026-11-18.tar.gz
   ```

   With the copy taken without stopping, the database goes back in its place:

   ```bash
   sudo tar -C / -xzf files.tar.gz
   sudo install -m 0600 alogeno.db /var/lib/alogeno/alogeno.db
   ```

3. Give the files to the `alogeno` user, whose numeric id can differ between machines:

   ```bash
   sudo chown -R alogeno:alogeno /var/lib/alogeno
   sudo chmod 0700 /var/lib/alogeno
   ```

4. Start and check:

   ```bash
   sudo systemctl start alogeno.socket alogeno.service
   journalctl -u alogeno -b --no-pager
   ```

   The log reports the database and its schema version, the result of the environment checks, and the address it listens on. Missing runtimes are downloaded again.

5. Point DNS and the reverse proxy to the new machine. Sessions stay valid, so signed-in people remain signed in. Without `agent/` in the copy, the owner connects the assistant again in Settings › Assistant.

## Testing the procedure

A backup that has never been restored is not known to work. Restoring a copy on a second machine, or in a virtual machine, and comparing the site, the pending changes and the published version with the original verifies both the copy and the procedure.
