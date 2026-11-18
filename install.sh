#!/usr/bin/env bash
# Installs alogeno on a Linux server with systemd.
#
#   curl -fsSL https://github.com/x71c9/alogeno/releases/latest/download/install.sh | sudo bash
#
# Environment:
#   ALOGENO_DOMAIN    domain of the site; the panel and the preview get
#                    panel.<domain> and preview.<domain>. Asked when missing.
#   ALOGENO_VERSION   release tag to install, for example v0.1.0 (default: latest)
#   ALOGENO_BASE_URL  where to download the release from (default: GitHub)
#   ALOGENO_PREFIX    install under this directory and leave systemd alone,
#                    for testing the installer without root
set -euo pipefail

REPO="x71c9/alogeno"

say() { printf 'alogeno: %s\n' "$*"; }
fail() { printf 'alogeno: %s\n' "$*" >&2; exit 1; }

# Everything runs from main, so a partly downloaded script does nothing.
main() {
  local prefix="${ALOGENO_PREFIX:-}"
  if [ -z "$prefix" ] && [ "$(id -u)" -ne 0 ]; then
    fail "run as root: curl -fsSL https://github.com/$REPO/releases/latest/download/install.sh | sudo bash"
  fi
  [ "$(uname -s)" = Linux ] || fail "alogeno runs on Linux only"
  if [ -z "$prefix" ] && [ ! -d /run/systemd/system ]; then
    fail "systemd is not the init system; alogeno runs as a systemd service"
  fi
  for tool in curl tar sha256sum; do
    command -v "$tool" >/dev/null || fail "$tool is required"
  done

  local arch
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "unsupported architecture $(uname -m); releases exist for x86_64 and aarch64" ;;
  esac

  local base
  if [ -n "${ALOGENO_BASE_URL:-}" ]; then
    base="$ALOGENO_BASE_URL"
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

  [ -n "$prefix" ] || install_bubblewrap

  say "installing into ${prefix:-/}"
  install -D -m 0755 "$release/alogeno" "$prefix/usr/local/bin/alogeno"
  install -D -m 0644 "$release/systemd/alogeno.service" "$prefix/etc/systemd/system/alogeno.service"
  install -D -m 0644 "$release/systemd/alogeno.socket" "$prefix/etc/systemd/system/alogeno.socket"
  install -D -m 0644 "$release/sysusers.d/alogeno.conf" "$prefix/etc/sysusers.d/alogeno.conf"
  install_apparmor "$release" "$prefix"
  write_config "$release" "$prefix"

  if [ -n "$prefix" ]; then
    say "installed under $prefix; systemd left untouched"
    return
  fi

  systemd-sysusers /etc/sysusers.d/alogeno.conf
  systemctl daemon-reload
  systemctl enable --now alogeno.socket
  systemctl enable alogeno.service

  say "checking the sandbox environment as the alogeno service would run"
  systemd-run --quiet --wait --pipe --collect \
    -p User=alogeno -p Delegate=yes -p DelegateSubgroup=main \
    /usr/local/bin/alogeno check || true

  # On a new installation the service prints the link that creates the
  # owner; take the latest one from its log.
  local owner="" since
  since="$(date +'%Y-%m-%d %H:%M:%S')"
  systemctl restart alogeno.service
  for _ in $(seq 1 20); do
    owner="$(journalctl -u alogeno --since "$since" --no-pager -o cat \
      | grep -o 'https\?://[^ ]*/setup#[^ ]*' | tail -n 1 || true)"
    [ -n "$owner" ] && break
    sleep 0.5
  done
  [ -n "$owner" ] || owner="(already created: sign in, or reset the password with the command below)"
  cat <<DONE

alogeno is running on 127.0.0.1:4100.

Next steps:
  1. Point the DNS of the panel, preview and site hostnames to this server.
  2. In the reverse proxy, forward those hostnames to 127.0.0.1:4100 with
     their original Host header, and set up HTTPS.
  3. Create the owner of the site with this one-time link (24 hours):

     $owner

     Restarting alogeno prints a new link until the owner exists. Lost the
     password later? Run, as root:
       runuser -u alogeno -- alogeno --config /etc/alogeno/alogeno.toml owner-link

Configuration: /etc/alogeno/alogeno.toml (then: systemctl restart alogeno)
Logs:          journalctl -u alogeno -f
DONE
}

install_bubblewrap() {
  if command -v bwrap >/dev/null; then
    return
  fi
  say "installing bubblewrap"
  if command -v apt-get >/dev/null; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bubblewrap
  elif command -v dnf >/dev/null; then
    dnf install -y -q bubblewrap
  elif command -v pacman >/dev/null; then
    pacman -S --noconfirm --needed bubblewrap
  elif command -v zypper >/dev/null; then
    zypper --non-interactive install bubblewrap
  else
    say "no known package manager: install bubblewrap by hand"
  fi
}

# Ubuntu 23.10 and later restrict user namespaces through AppArmor.
install_apparmor() {
  local release="$1" prefix="$2"
  local restrict=/proc/sys/kernel/apparmor_restrict_unprivileged_userns
  [ -r "$restrict" ] && [ "$(cat "$restrict")" = 1 ] || return 0
  if grep -lsq '/usr/bin/bwrap' "$prefix"/etc/apparmor.d/* 2>/dev/null; then
    say "an AppArmor profile for bwrap already exists; leaving it as it is"
    return
  fi
  install -D -m 0644 "$release/apparmor/alogeno-bwrap" "$prefix/etc/apparmor.d/alogeno-bwrap"
  if [ -z "$prefix" ] && command -v apparmor_parser >/dev/null; then
    apparmor_parser -r /etc/apparmor.d/alogeno-bwrap
  fi
}

write_config() {
  local release="$1" prefix="$2"
  local config="$prefix/etc/alogeno/alogeno.toml"
  if [ -f "$config" ]; then
    say "keeping the existing $config"
    return
  fi
  local domain="${ALOGENO_DOMAIN:-}"
  if [ -z "$domain" ] && [ -r /dev/tty ]; then
    printf 'alogeno: domain of the site (for example example.com): ' >/dev/tty
    read -r domain </dev/tty
  fi
  domain="$(printf '%s' "$domain" | tr '[:upper:]' '[:lower:]' | sed 's#^https\?://##; s#/.*##')"
  [ -n "$domain" ] || fail "no domain given; set ALOGENO_DOMAIN=example.com"
  install -d -m 0755 "$(dirname "$config")"
  sed -e "s/panel.example.com/panel.$domain/" \
      -e "s/preview.example.com/preview.$domain/" \
      -e "s/\"example.com\", \"www.example.com\"/\"$domain\", \"www.$domain\"/" \
      "$release/alogeno.toml" > "$config"
  chmod 0644 "$config"
  say "wrote $config for $domain"
}

main "$@"
