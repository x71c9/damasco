#!/usr/bin/env bash
# Installs axioma on a Linux server with systemd.
#
#   curl -fsSL https://github.com/x71c9/axioma/releases/latest/download/install.sh | sudo bash
#
# Environment:
#   AXIOMA_DOMAIN    domain of the site; the panel and the preview get
#                    panel.<domain> and preview.<domain>. Asked when missing.
#   AXIOMA_VERSION   release tag to install, for example v0.1.0 (default: latest)
#   AXIOMA_BASE_URL  where to download the release from (default: GitHub)
#   AXIOMA_PREFIX    install under this directory and leave systemd alone,
#                    for testing the installer without root
set -euo pipefail

REPO="x71c9/axioma"

say() { printf 'axioma: %s\n' "$*"; }
fail() { printf 'axioma: %s\n' "$*" >&2; exit 1; }

# Everything runs from main, so a partly downloaded script does nothing.
main() {
  local prefix="${AXIOMA_PREFIX:-}"
  if [ -z "$prefix" ] && [ "$(id -u)" -ne 0 ]; then
    fail "run as root: curl -fsSL https://github.com/$REPO/releases/latest/download/install.sh | sudo bash"
  fi
  [ "$(uname -s)" = Linux ] || fail "axioma runs on Linux only"
  if [ -z "$prefix" ] && [ ! -d /run/systemd/system ]; then
    fail "systemd is not the init system; axioma runs as a systemd service"
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
  if [ -n "${AXIOMA_BASE_URL:-}" ]; then
    base="$AXIOMA_BASE_URL"
  elif [ -n "${AXIOMA_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$AXIOMA_VERSION"
  else
    base="https://github.com/$REPO/releases/latest/download"
  fi

  WORK="$(mktemp -d)"
  trap 'rm -rf -- "$WORK"' EXIT
  local work="$WORK"
  local archive="axioma-$arch-linux.tar.gz"
  say "downloading $archive"
  curl -fsSL "$base/$archive" -o "$work/$archive"
  curl -fsSL "$base/SHA256SUMS" -o "$work/SHA256SUMS"
  (cd "$work" && grep " $archive\$" SHA256SUMS | sha256sum --check --quiet) \
    || fail "checksum of $archive does not match SHA256SUMS"
  tar -xzf "$work/$archive" -C "$work"
  local release="$work/axioma-$arch-linux"

  [ -n "$prefix" ] || install_bubblewrap

  say "installing into ${prefix:-/}"
  install -D -m 0755 "$release/axioma" "$prefix/usr/local/bin/axioma"
  install -D -m 0644 "$release/systemd/axioma.service" "$prefix/etc/systemd/system/axioma.service"
  install -D -m 0644 "$release/systemd/axioma.socket" "$prefix/etc/systemd/system/axioma.socket"
  install -D -m 0644 "$release/sysusers.d/axioma.conf" "$prefix/etc/sysusers.d/axioma.conf"
  install_apparmor "$release" "$prefix"
  write_config "$release" "$prefix"

  if [ -n "$prefix" ]; then
    say "installed under $prefix; systemd left untouched"
    return
  fi

  systemd-sysusers /etc/sysusers.d/axioma.conf
  systemctl daemon-reload
  systemctl enable --now axioma.socket
  systemctl enable axioma.service
  systemctl restart axioma.service

  say "checking the sandbox environment as the axioma service would run"
  systemd-run --quiet --wait --pipe --collect \
    -p User=axioma -p Delegate=yes -p DelegateSubgroup=main \
    /usr/local/bin/axioma check || true

  local owner
  owner="$(runuser -u axioma -- /usr/local/bin/axioma --config /etc/axioma/axioma.toml owner-link)"
  cat <<DONE

axioma is running on 127.0.0.1:4100.

Next steps:
  1. Point the DNS of the panel, preview and site hostnames to this server.
  2. In the reverse proxy, forward those hostnames to 127.0.0.1:4100 with
     their original Host header, and set up HTTPS.
  3. Create the owner of the site with this link:

$owner

     Lost the password later? Run, as root:
       runuser -u axioma -- axioma --config /etc/axioma/axioma.toml owner-link

Configuration: /etc/axioma/axioma.toml (then: systemctl restart axioma)
Logs:          journalctl -u axioma -f
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
  install -D -m 0644 "$release/apparmor/axioma-bwrap" "$prefix/etc/apparmor.d/axioma-bwrap"
  if [ -z "$prefix" ] && command -v apparmor_parser >/dev/null; then
    apparmor_parser -r /etc/apparmor.d/axioma-bwrap
  fi
}

write_config() {
  local release="$1" prefix="$2"
  local config="$prefix/etc/axioma/axioma.toml"
  if [ -f "$config" ]; then
    say "keeping the existing $config"
    return
  fi
  local domain="${AXIOMA_DOMAIN:-}"
  if [ -z "$domain" ] && [ -r /dev/tty ]; then
    printf 'axioma: domain of the site (for example example.com): ' >/dev/tty
    read -r domain </dev/tty
  fi
  domain="$(printf '%s' "$domain" | tr '[:upper:]' '[:lower:]' | sed 's#^https\?://##; s#/.*##')"
  [ -n "$domain" ] || fail "no domain given; set AXIOMA_DOMAIN=example.com"
  install -d -m 0755 "$(dirname "$config")"
  sed -e "s/panel.example.com/panel.$domain/" \
      -e "s/preview.example.com/preview.$domain/" \
      -e "s/\"example.com\", \"www.example.com\"/\"$domain\", \"www.$domain\"/" \
      "$release/axioma.toml" > "$config"
  chmod 0644 "$config"
  say "wrote $config for $domain"
}

main "$@"
