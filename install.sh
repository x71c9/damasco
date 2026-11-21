#!/usr/bin/env bash
# Installs or upgrades alogeno on a Linux server with systemd.
#
#   curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
#
# Running it again upgrades to the latest release; configuration and data stay
# as they are. One installation serves several sites from one panel. One
# machine can hold several installations, each with its own sites, data,
# system user and port: ALOGENO_INSTANCE names an installation besides the
# default one. The binary is shared, so an upgrade restarts every installation
# on the machine.
#
# Environment. sudo drops the variables of the caller, so they go after it:
#   curl -fsSL .../install.sh | sudo ALOGENO_PANEL=panel.example.com bash
#
#   ALOGENO_PANEL     domain of the panel, for example panel.example.com; the
#                     preview of each site gets <slug>.preview.<panel>. Asked
#                     when missing on a new installation.
#   ALOGENO_INSTANCE  name of an installation besides the default one, for
#                     example blog: units alogeno@blog.*, user alogeno-blog,
#                     /etc/alogeno-blog, /var/lib/alogeno-blog. router is
#                     reserved
#   ALOGENO_PORT      local port of a new installation (default: 4100 for the
#                     default one, the first free one from 4101 for the others)
#   ALOGENO_ASK_PORT  how a new installation gets its domains to the reverse
#                     proxy. A port: Caddy asks alogeno on it before getting a
#                     certificate, so domains added in the panel work at once
#                     (default: 4099 for the default installation). none: the
#                     names go into the proxy by hand (the default for named
#                     installations; also for nginx). One installation per
#                     machine asks on a port
#   ALOGENO_UPGRADE   1: only upgrade the binary and the units, and restart
#                     every installation; never create one (alogeno update)
#   ALOGENO_VERSION   release tag to install, for example v0.1.0 (default: latest)
#   ALOGENO_BASE_URL  where to download the release from (default: GitHub)
#   ALOGENO_PREFIX    install under this directory and leave systemd alone,
#                     for testing the installer without root
set -euo pipefail

REPO="x71c9/alogeno"
DOCS="https://github.com/$REPO/blob/master/docs"
# DelegateSubgroup= in alogeno.service.
MIN_SYSTEMD=254

say() { printf 'alogeno: %s\n' "$*"; }
fail() { printf 'alogeno: %s\n' "$*" >&2; exit 1; }

# Everything runs from main, so a partly downloaded script does nothing.
main() {
  local prefix="${ALOGENO_PREFIX:-}" upgrade="${ALOGENO_UPGRADE:-}"
  if [ -z "$prefix" ] && [ "$(id -u)" -ne 0 ]; then
    fail "run as root, for example: curl -fsSL <installer> | sudo bash"
  fi
  [ "$(uname -s)" = Linux ] || fail "alogeno runs on Linux only"
  [ -n "$prefix" ] || check_systemd
  for tool in curl tar gzip sha256sum; do
    command -v "$tool" >/dev/null || fail "$tool is required"
  done

  local arch
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "unsupported architecture $(uname -m); releases exist for x86_64 and aarch64" ;;
  esac

  # The installation this run is about; none in upgrade mode.
  local instance="${ALOGENO_INSTANCE:-}"
  if [ -n "$instance" ] && ! [[ "$instance" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]]; then
    fail "ALOGENO_INSTANCE=$instance: use lower-case letters, digits and -, up to 32"
  fi
  [ "$instance" != router ] || fail "ALOGENO_INSTANCE=router is reserved; choose another name"
  [ -z "$upgrade" ] || [ -z "$instance" ] || fail "ALOGENO_UPGRADE upgrades every installation: leave out ALOGENO_INSTANCE"
  names "$instance"

  WORK="$(mktemp -d)"
  trap 'rm -rf -- "$WORK"' EXIT
  local work="$WORK"

  local config="$prefix$CONFIG" panel="" port="" ask=""
  if [ -z "$upgrade" ] && [ ! -f "$config" ]; then
    # Asked before anything is written; an upgrade keeps its configuration.
    panel="$(ask_panel)"
    port="$(free_port "$prefix")"
    ask="$(ask_port "$prefix" "$port")"
  fi

  local base
  if [ -n "${ALOGENO_BASE_URL:-}" ]; then
    base="${ALOGENO_BASE_URL%/}"
  elif [ -n "${ALOGENO_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$ALOGENO_VERSION"
  else
    base="https://github.com/$REPO/releases/latest/download"
  fi

  local archive="alogeno-$arch-linux.tar.gz"
  say "downloading $archive"
  curl -fsSL "$base/$archive" -o "$work/$archive"
  curl -fsSL "$base/SHA256SUMS" -o "$work/SHA256SUMS"
  (cd "$work" && grep " $archive\$" SHA256SUMS | sha256sum --check --quiet) \
    || fail "checksum of $archive does not match SHA256SUMS"
  tar -xzf "$work/$archive" -C "$work"
  local release="$work/alogeno-$arch-linux"

  local binary="$prefix/usr/local/bin/alogeno" previous="" version
  if [ -x "$binary" ]; then
    previous="$("$binary" --version 2>/dev/null | awk '{print "v" $2}' || true)"
  fi
  version="$("$release/alogeno" --version | awk '{print "v" $2}')"
  if [ -n "$previous" ]; then
    say "upgrading $previous to $version"
  else
    say "installing $version"
  fi

  [ -n "$prefix" ] || install_packages

  install -D -m 0755 "$release/alogeno" "$binary"
  install_units "$release" "$prefix"
  install_apparmor "$release" "$prefix"
  if [ -z "$upgrade" ]; then
    write_config "$release" "$config" "$panel" "$port" "$ask"
    write_instance "$prefix" "$port"
  fi

  if [ -n "$prefix" ]; then
    say "installed under $prefix; systemd left untouched"
    return
  fi

  systemctl daemon-reload
  if [ -n "$upgrade" ]; then
    restart_all ""
    finish_upgrade "$version"
    return
  fi

  systemd-sysusers "$SYSUSERS"
  systemctl enable --quiet --now "$SOCKET"
  systemctl enable --quiet "$SERVICE"
  if [ -f "/etc/systemd/system/$PATH_TEMPLATE" ]; then
    systemctl enable --quiet --now "$PATH_UNIT"
  fi
  # The binary is shared: the other installations run the new one too.
  restart_all "$SERVICE"

  say "starting $SERVICE"
  systemctl restart "$SERVICE"
  local owner listen
  owner="$(wait_for_start "$SERVICE")"
  listen="$(listen_of "$SOCKET")"

  if [ -n "$previous" ] && [ -z "$panel" ]; then
    printf '\nalogeno %s is running on %s.\n' "$version" "$listen"
    if [ -n "$owner" ]; then
      printf '\nThe owner does not exist yet; create it with this one-time link (24 hours):\n\n  %s\n' "$owner"
    fi
    finish
    return
  fi

  [ -n "$owner" ] || owner="(not printed: see journalctl -u $SERVICE, or run the owner-link command below)"
  local proxy
  if [ -n "$ask" ]; then
    proxy="$(cat <<PROXY
  2. Caddy serves the panel, the previews and every domain added in the
     panel, asking alogeno before getting each certificate. In the Caddyfile:

       {
         on_demand_tls {
           ask http://127.0.0.1:$ask/ask
         }
       }

       https:// {
         tls {
           on_demand
         }
         reverse_proxy $listen
       }

     $DOCS/install.md#reverse-proxy
PROXY
)"
  else
    proxy="$(cat <<PROXY
  2. In the reverse proxy, forward $panel and the preview of
     each site, starting with main.preview.$panel, to $listen
     with their original Host header, and set up HTTPS. Each new site and
     domain goes into the proxy the same way; Settings › Sites lists them:
     $DOCS/install.md#reverse-proxy
PROXY
)"
  fi
  cat <<DONE

alogeno $version is running on $listen.

Next steps:
  1. Point the DNS of $panel and of the previews, *.preview.$panel,
     to this server.
$proxy
  3. Create the owner of the installation with this one-time link (24 hours):

     $owner

     Restarting alogeno prints a new link until the owner exists. Lost the
     password later? Run, as root:
       runuser -u $USER_NAME -- alogeno --config $CONFIG owner-link
DONE
  finish
}

# The names of one installation: the default one when $1 is empty.
names() {
  local instance="$1"
  if [ -z "$instance" ]; then
    SERVICE=alogeno.service
    SOCKET=alogeno.socket
    PATH_UNIT=alogeno-update.path
    PATH_TEMPLATE=alogeno-update.path
    USER_NAME=alogeno
    CONFIG=/etc/alogeno/alogeno.toml
    DATA=/var/lib/alogeno
    SYSUSERS=/etc/sysusers.d/alogeno.conf
  else
    SERVICE="alogeno@$instance.service"
    SOCKET="alogeno@$instance.socket"
    PATH_UNIT="alogeno-update@$instance.path"
    PATH_TEMPLATE="alogeno-update@.path"
    USER_NAME="alogeno-$instance"
    CONFIG="/etc/alogeno-$instance/alogeno.toml"
    DATA="/var/lib/alogeno-$instance"
    SYSUSERS="/etc/sysusers.d/alogeno-$instance.conf"
  fi
}

# Every installation on the machine, by its service: one per configuration.
installations() {
  [ ! -f /etc/alogeno/alogeno.toml ] || echo alogeno.service
  local config name
  for config in /etc/alogeno-*/alogeno.toml; do
    [ -f "$config" ] || continue
    name="${config#/etc/alogeno-}"
    echo "alogeno@${name%/alogeno.toml}.service"
  done
}

# Restarts every installation but $1 that is running, so that all of them
# run the binary just installed. In upgrade mode ($1 empty) it waits for
# each and shows its checks.
restart_all() {
  local skip="$1" service
  for service in $(installations); do
    [ "$service" != "$skip" ] || continue
    systemctl is-enabled --quiet "$service" 2>/dev/null || continue
    say "restarting $service"
    systemctl restart "$service"
    if [ -z "$skip" ]; then
      wait_for_start "$service" >/dev/null
    fi
  done
}

finish_upgrade() {
  printf '\nalogeno %s is installed; every installation on this machine runs it.\n' "$1"
  printf 'Logs: journalctl -u alogeno -u '"'"'alogeno@*'"'"' -f\n'
}

finish() {
  cat <<DONE

Configuration: $CONFIG (then: systemctl restart $SERVICE)
Data:          $DATA (backups: $DOCS/backup.md)
Logs:          journalctl -u $SERVICE -f
DONE
}

check_systemd() {
  [ -d /run/systemd/system ] || fail "systemd is not the init system; alogeno runs as a systemd service"
  local have
  have="$(systemctl --version | awk 'NR == 1 {print $2}')"
  have="${have%%[!0-9]*}"
  if [ -z "$have" ] || [ "$have" -lt "$MIN_SYSTEMD" ]; then
    fail "systemd ${have:-of unknown version} is too old; alogeno needs systemd $MIN_SYSTEMD or later (for example Debian 13, Ubuntu 24.04, Fedora 40)"
  fi
}

# Units of the release: the default installation, the templates of the named
# ones, and the updates asked from the panel. Older releases miss some.
install_units() {
  local release="$1" prefix="$2" unit
  for unit in alogeno.service alogeno.socket alogeno@.service alogeno@.socket \
    alogeno-update.service alogeno-update.path alogeno-update@.path; do
    [ ! -f "$release/systemd/$unit" ] \
      || install -D -m 0644 "$release/systemd/$unit" "$prefix/etc/systemd/system/$unit"
  done
  install -D -m 0644 "$release/sysusers.d/alogeno.conf" "$prefix/etc/sysusers.d/alogeno.conf"
}

# The parts of a new named installation that the templates cannot carry:
# its system user and its port.
write_instance() {
  local prefix="$1" port="$2"
  [ -n "$port" ] || return 0
  if [ "$SERVICE" != alogeno.service ]; then
    printf 'u %s - "alogeno %s" %s\n' "$USER_NAME" "${USER_NAME#alogeno-}" "$DATA" \
      | install -D -m 0644 /dev/stdin "$prefix$SYSUSERS"
  fi
  [ "$SERVICE" != alogeno.service ] || [ "$port" != 4100 ] || return 0
  printf '[Socket]\nListenStream=\nListenStream=127.0.0.1:%s\n' "$port" \
    | install -D -m 0644 /dev/stdin "$prefix/etc/systemd/system/$SOCKET.d/listen.conf"
}

# ALOGENO_PORT, or the first port no installation and no other program uses:
# 4100 for the default installation, from 4101 for the others.
free_port() {
  local prefix="$1" port taken
  if [ -n "${ALOGENO_PORT:-}" ]; then
    if ! [[ "$ALOGENO_PORT" =~ ^[0-9]+$ ]] || [ "$ALOGENO_PORT" -lt 1024 ] || [ "$ALOGENO_PORT" -gt 65535 ]; then
      fail "ALOGENO_PORT=$ALOGENO_PORT is not a port between 1024 and 65535"
    fi
    printf '%s' "$ALOGENO_PORT"
    return
  fi
  if [ "$SERVICE" = alogeno.service ]; then
    printf '4100'
    return
  fi
  taken="$(cat "$prefix"/etc/systemd/system/alogeno*.socket.d/*.conf "$prefix"/etc/systemd/system/alogeno.socket 2>/dev/null \
    | sed -n 's/^ListenStream=.*:\([0-9]*\)$/\1/p')"
  for port in $(seq 4101 4199); do
    grep -qx "$port" <<<"$taken" && continue
    if [ -z "$prefix" ] && command -v ss >/dev/null && [ -n "$(ss -Hltn "sport = :$port")" ]; then
      continue
    fi
    printf '%s' "$port"
    return
  done
  fail "no free port between 4101 and 4199; set ALOGENO_PORT"
}

# The address the socket of an installation listens on.
listen_of() {
  systemctl show -p Listen --value "$1" | awk '{print $1; exit}'
}

# bubblewrap runs the sandbox, git keeps the history of the site, the
# certificates let the sandbox reach HTTPS hosts, and the Node.js that
# alogeno downloads needs libatomic from the host.
install_packages() {
  local missing=() atomic="" manager
  command -v bwrap >/dev/null || missing+=(bubblewrap)
  command -v git >/dev/null || missing+=(git)
  [ -d /etc/ssl/certs ] || [ -d /etc/pki/tls/certs ] || missing+=(ca-certificates)
  ldconfig -p 2>/dev/null | grep -q 'libatomic\.so\.1 ' || atomic=1
  for manager in apt-get dnf pacman zypper ""; do
    [ -z "$manager" ] || command -v "$manager" >/dev/null && break
  done
  if [ -n "$atomic" ]; then
    case "$manager" in
      apt-get | zypper) missing+=(libatomic1) ;;
      pacman) missing+=(gcc-libs) ;;
      *) missing+=(libatomic) ;;
    esac
  fi
  [ "${#missing[@]}" -gt 0 ] || return 0
  [ -n "$manager" ] || fail "no known package manager: install ${missing[*]}, then run the installer again"
  say "installing ${missing[*]}"
  # Quiet unless it fails: package managers and their hooks talk a lot.
  if ! case "$manager" in
    apt-get) apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "${missing[@]}" ;;
    dnf) dnf install -y -q "${missing[@]}" ;;
    pacman) pacman -S --noconfirm --needed "${missing[@]}" ;;
    zypper) zypper --non-interactive --quiet install "${missing[@]}" ;;
  esac >"$WORK/packages.log" 2>&1; then
    cat "$WORK/packages.log" >&2
    fail "$manager could not install ${missing[*]}"
  fi
}

# Ubuntu 23.10 and later restrict user namespaces through AppArmor.
install_apparmor() {
  local release="$1" prefix="$2"
  local restrict=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
  [ -r "$restrict" ] && [ "$(cat "$restrict")" = 1 ] || return 0
  if [ ! -f "$prefix/etc/apparmor.d/alogeno-bwrap" ] \
    && grep -lsq '/usr/bin/bwrap' "$prefix"/etc/apparmor.d/* 2>/dev/null; then
    say "an AppArmor profile for bwrap already exists; leaving it as it is"
    return
  fi
  install -D -m 0644 "$release/apparmor/alogeno-bwrap" "$prefix/etc/apparmor.d/alogeno-bwrap"
  if [ -z "$prefix" ] && command -v apparmor_parser >/dev/null; then
    apparmor_parser -r /etc/apparmor.d/alogeno-bwrap
  fi
}

# Prints the domain of the panel, from ALOGENO_PANEL or asked on the terminal.
ask_panel() {
  local panel="${ALOGENO_PANEL:-}"
  if [ -z "$panel" ] && { : </dev/tty; } 2>/dev/null; then
    printf 'alogeno: domain of the panel (for example panel.example.com): ' >/dev/tty
    read -r panel </dev/tty
  fi
  panel="$(printf '%s' "$panel" | tr '[:upper:]' '[:lower:]' | sed 's#^https\?://##; s#/.*##')"
  [ -n "$panel" ] || fail "no domain given; run again with: sudo ALOGENO_PANEL=panel.example.com bash"
  [[ "$panel" =~ ^[a-z0-9-]+(\.[a-z0-9-]+)+$ ]] || fail "$panel is not a domain such as panel.example.com"
  printf '%s' "$panel"
}

# The port where the reverse proxy asks this new installation whether it
# serves a name (Caddy's on-demand TLS), or nothing when names go into the
# proxy by hand. From ALOGENO_ASK_PORT: unset means 4099 for the default
# installation and by hand for the others; none means by hand. One
# installation per machine asks on a port: the proxy has one question.
ask_port() {
  local prefix="$1" listen_port="$2" ask other
  if [ -n "${ALOGENO_ASK_PORT+set}" ]; then
    ask="$ALOGENO_ASK_PORT"
  elif [ "$SERVICE" = alogeno.service ]; then
    ask=4099
  else
    ask=none
  fi
  [ "$ask" != none ] || return 0
  if ! [[ "$ask" =~ ^[0-9]+$ ]] || [ "$ask" -lt 1024 ] || [ "$ask" -gt 65535 ]; then
    fail "ALOGENO_ASK_PORT=$ask is not a port between 1024 and 65535, nor none"
  fi
  for other in "$prefix"/etc/alogeno/alogeno.toml "$prefix"/etc/alogeno-*/alogeno.toml; do
    [ -f "$other" ] && [ "$other" != "$prefix$CONFIG" ] || continue
    if grep -q '^ask_listen' "$other"; then
      fail "the installation in $(dirname "$other") already receives the domains automatically; run again with ALOGENO_ASK_PORT=none and add the names to the proxy by hand"
    fi
  done
  [ "$ask" != "$listen_port" ] || fail "ALOGENO_ASK_PORT=$ask is the port of the installation itself; choose another"
  if [ -z "$prefix" ] && command -v ss >/dev/null && [ -n "$(ss -Hltn "sport = :$ask")" ]; then
    fail "port $ask is in use; choose another with ALOGENO_ASK_PORT"
  fi
  printf '%s' "$ask"
}

write_config() {
  local release="$1" config="$2" panel="$3" port="$4" ask="$5"
  if [ -f "$config" ]; then
    say "keeping $config"
    return
  fi
  install -d -m 0755 "$(dirname "$config")"
  sed -e "s/panel.example.com/$panel/" \
      -e "s#^listen = .*#listen = \"127.0.0.1:$port\"#" \
      -e "s#^data_dir = .*#data_dir = \"$DATA\"#" \
      "$release/alogeno.toml" > "$config"
  if [ -n "$ask" ]; then
    printf '\n[domains]\n# Caddy asks here whether a name is served, before getting its certificate.\nask_listen = "127.0.0.1:%s"\n' "$ask" >> "$config"
  fi
  chmod 0644 "$config"
  say "wrote $config for $panel"
}

# Waits until the service $1 listens, shows the result of the environment
# checks it ran at start (on stderr), and prints the owner's link when this
# start printed one. Fails with the log when the service stops.
wait_for_start() {
  local service="$1" invocation log
  invocation="$(systemctl show -p InvocationID --value "$service")"
  for _ in $(seq 1 60); do
    log="$(journalctl --no-pager -o cat "_SYSTEMD_INVOCATION_ID=$invocation" 2>/dev/null || true)"
    if grep -q 'alogeno: listening on' <<<"$log"; then
      grep -E '^alogeno: (environment ready|FAIL|warn|the assistant and the builds cannot run)' <<<"$log" >&2 || true
      grep -o 'https\?://[^ ]*/setup#[^ ]*' <<<"$log" | tail -n 1 || true
      return
    fi
    if systemctl is-failed --quiet "$service"; then
      journalctl --no-pager -u "$service" -n 30 >&2
      fail "$service stopped; the log is above"
    fi
    sleep 0.5
  done
  say "$service did not report that it listens within 30 seconds; see journalctl -u $service" >&2
}

main "$@"
