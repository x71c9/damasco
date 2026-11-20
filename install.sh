#!/usr/bin/env bash
# Installs or upgrades alogeno on a Linux server with systemd.
#
#   curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
#
# Running it again upgrades to the latest release; configuration and data stay
# as they are. One machine can hold several installations, each with its own
# site, data, system user and port: ALOGENO_INSTANCE names an installation
# besides the default one. The binary is shared, so an upgrade restarts every
# installation on the machine.
#
# Environment. sudo drops the variables of the caller, so they go after it:
#   curl -fsSL .../install.sh | sudo ALOGENO_DOMAIN=example.com bash
#
#   ALOGENO_DOMAIN    domain of the site; the panel and the preview get
#                     panel.<domain> and preview.<domain>. Asked when missing
#                     on a new installation.
#   ALOGENO_INSTANCE  name of an installation besides the default one, for
#                     example blog: units alogeno@blog.*, user alogeno-blog,
#                     /etc/alogeno-blog, /var/lib/alogeno-blog
#   ALOGENO_PORT      local port of a new installation (default: 4100 for the
#                     default one, the first free one from 4101 for the others)
#   ALOGENO_UPGRADE   1: only upgrade the binary and the units, and restart
#                     every installation; never create one (alogeno update)
#   ALOGENO_VERSION   release tag to install, for example v0.1.0 (default: latest)
#   ALOGENO_BASE_URL  where to download the release from (default: GitHub)
#   ALOGENO_PREFIX    install under this directory and leave systemd alone,
#                     for testing the installer without root
set -euo pipefail

REPO="x71c9/alogeno"
DOCS="https://github.com/$REPO/blob/1.x/docs"
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
  [ -z "$upgrade" ] || [ -z "$instance" ] || fail "ALOGENO_UPGRADE upgrades every installation: leave out ALOGENO_INSTANCE"
  names "$instance"
  local config="$prefix$CONFIG" domain="" port=""
  if [ -z "$upgrade" ] && [ ! -f "$config" ]; then
    # Asked before anything is written; an upgrade keeps its configuration.
    domain="$(ask_domain)"
    port="$(free_port "$prefix")"
  fi

  local base
  if [ -n "${ALOGENO_BASE_URL:-}" ]; then
    base="${ALOGENO_BASE_URL%/}"
  elif [ -n "${ALOGENO_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$ALOGENO_VERSION"
  else
    base="https://github.com/$REPO/releases/latest/download"
  fi

  WORK="$(mktemp -d)"
  trap 'rm -rf -- "$WORK"' EXIT
  local work="$WORK"
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
    write_config "$release" "$config" "$domain" "$port"
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

  if [ -n "$previous" ] && [ -z "$domain" ]; then
    printf '\nalogeno %s is running on %s.\n' "$version" "$listen"
    if [ -n "$owner" ]; then
      printf '\nThe owner does not exist yet; create it with this one-time link (24 hours):\n\n  %s\n' "$owner"
    fi
    finish
    return
  fi

  [ -n "$owner" ] || owner="(not printed: see journalctl -u $SERVICE, or run the owner-link command below)"
  cat <<DONE

alogeno $version is running on $listen.

Next steps:
  1. Point the DNS of panel.$domain and of the previews, *.preview.panel.$domain,
     to this server.
  2. In the reverse proxy, forward those hostnames to $listen with
     their original Host header, and set up HTTPS:
     $DOCS/install.md#reverse-proxy
  3. Create the owner of the site with this one-time link (24 hours):

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

# Prints the domain of the site, from ALOGENO_DOMAIN or asked on the terminal.
ask_domain() {
  local domain="${ALOGENO_DOMAIN:-}"
  if [ -z "$domain" ] && { : </dev/tty; } 2>/dev/null; then
    printf 'alogeno: domain of the site (for example example.com): ' >/dev/tty
    read -r domain </dev/tty
  fi
  domain="$(printf '%s' "$domain" | tr '[:upper:]' '[:lower:]' | sed 's#^https\?://##; s#/.*##')"
  [ -n "$domain" ] || fail "no domain given; run again with: sudo ALOGENO_DOMAIN=example.com bash"
  [[ "$domain" =~ ^[a-z0-9-]+(\.[a-z0-9-]+)+$ ]] || fail "$domain is not a domain such as example.com"
  printf '%s' "$domain"
}

write_config() {
  local release="$1" config="$2" domain="$3" port="$4"
  if [ -f "$config" ]; then
    say "keeping $config"
    return
  fi
  install -d -m 0755 "$(dirname "$config")"
  sed -e "s/panel.example.com/panel.$domain/" \
      -e "s#^listen = .*#listen = \"127.0.0.1:$port\"#" \
      -e "s#^data_dir = .*#data_dir = \"$DATA\"#" \
      "$release/alogeno.toml" > "$config"
  chmod 0644 "$config"
  say "wrote $config for $domain"
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
