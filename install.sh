#!/usr/bin/env bash
# Installs or upgrades alogeno on a Linux server with systemd.
#
#   curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
#
# Running it again upgrades to the latest release; the configuration and the
# data stay as they are.
#
# Environment. sudo drops the variables of the caller, so they go after it:
#   curl -fsSL .../install.sh | sudo ALOGENO_DOMAIN=example.com bash
#
#   ALOGENO_DOMAIN    domain of the site; the panel and the preview get
#                     panel.<domain> and preview.<domain>. Asked when missing
#                     on a new installation.
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
  local prefix="${ALOGENO_PREFIX:-}"
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

  # Asked before anything is written; an upgrade keeps its configuration.
  local config="$prefix/etc/alogeno/alogeno.toml" domain=""
  [ -f "$config" ] || domain="$(ask_domain)"

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
    previous="$("$binary" --version 2>/dev/null | awk '{print $2}' || true)"
  fi
  version="$("$release/alogeno" --version | awk '{print $2}')"
  if [ -n "$previous" ]; then
    say "upgrading $previous to $version"
  else
    say "installing $version"
  fi

  [ -n "$prefix" ] || install_packages

  install -D -m 0755 "$release/alogeno" "$binary"
  install -D -m 0644 "$release/systemd/alogeno.service" "$prefix/etc/systemd/system/alogeno.service"
  install -D -m 0644 "$release/systemd/alogeno.socket" "$prefix/etc/systemd/system/alogeno.socket"
  install -D -m 0644 "$release/sysusers.d/alogeno.conf" "$prefix/etc/sysusers.d/alogeno.conf"
  install_apparmor "$release" "$prefix"
  write_config "$release" "$config" "$domain"

  if [ -n "$prefix" ]; then
    say "installed under $prefix; systemd left untouched"
    return
  fi

  systemd-sysusers /etc/sysusers.d/alogeno.conf
  systemctl daemon-reload
  systemctl enable --quiet --now alogeno.socket
  systemctl enable --quiet alogeno.service

  say "starting the service"
  systemctl restart alogeno.service
  local owner
  owner="$(wait_for_start)"

  if [ -n "$previous" ]; then
    printf '\nalogeno %s is running on 127.0.0.1:4100.\n' "$version"
    if [ -n "$owner" ]; then
      printf '\nThe owner does not exist yet; create it with this one-time link (24 hours):\n\n  %s\n' "$owner"
    fi
    finish
    return
  fi

  [ -n "$owner" ] || owner="(not printed: see journalctl -u alogeno, or run the owner-link command below)"
  cat <<DONE

alogeno $version is running on 127.0.0.1:4100.

Next steps:
  1. Point the DNS of the panel, preview and site hostnames to this server.
  2. In the reverse proxy, forward those hostnames to 127.0.0.1:4100 with
     their original Host header, and set up HTTPS:
     $DOCS/install.md#reverse-proxy
  3. Create the owner of the site with this one-time link (24 hours):

     $owner

     Restarting alogeno prints a new link until the owner exists. Lost the
     password later? Run, as root:
       runuser -u alogeno -- alogeno --config /etc/alogeno/alogeno.toml owner-link
DONE
  finish
}

finish() {
  cat <<DONE

Configuration: /etc/alogeno/alogeno.toml (then: systemctl restart alogeno)
Data:          /var/lib/alogeno (backups: $DOCS/backup.md)
Logs:          journalctl -u alogeno -f
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
  local release="$1" config="$2" domain="$3"
  if [ -f "$config" ]; then
    say "keeping $config"
    return
  fi
  install -d -m 0755 "$(dirname "$config")"
  sed -e "s/panel.example.com/panel.$domain/" \
      -e "s/preview.example.com/preview.$domain/" \
      -e "s/\"example.com\", \"www.example.com\"/\"$domain\", \"www.$domain\"/" \
      "$release/alogeno.toml" > "$config"
  chmod 0644 "$config"
  say "wrote $config for $domain"
}

# Waits until the service listens, shows the result of the environment checks
# the service ran at start (on stderr), and prints the owner's link when this
# start printed one. Fails with the log when the service stops.
wait_for_start() {
  local invocation log
  invocation="$(systemctl show -p InvocationID --value alogeno.service)"
  for _ in $(seq 1 60); do
    log="$(journalctl --no-pager -o cat "_SYSTEMD_INVOCATION_ID=$invocation" 2>/dev/null || true)"
    if grep -q 'alogeno: listening on' <<<"$log"; then
      grep -E '^alogeno: (environment ready|FAIL|warn|the sandbox stays disabled)' <<<"$log" >&2 || true
      grep -o 'https\?://[^ ]*/setup#[^ ]*' <<<"$log" | tail -n 1 || true
      return
    fi
    if systemctl is-failed --quiet alogeno.service; then
      journalctl --no-pager -u alogeno -n 30 >&2
      fail "the service stopped; the log is above"
    fi
    sleep 0.5
  done
  say "the service did not report that it listens within 30 seconds; see journalctl -u alogeno" >&2
}

main "$@"
