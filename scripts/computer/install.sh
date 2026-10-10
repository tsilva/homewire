#!/bin/sh
# Run as root on the target. The source configuration contains a public client key.
set -eu
[ "$(id -u)" = 0 ] || { echo 'Run as root' >&2; exit 1; }
[ "$#" = 3 ] || { echo 'Usage: install.sh BINARY SHA256 SERVER_JSON' >&2; exit 1; }
binary=$1
digest=$2
config=$3
case "$digest" in *[!a-f0-9]*|'') echo 'Invalid digest' >&2; exit 1 ;; esac
[ "${#digest}" = 64 ]
[ "$(sha256sum "$binary" | cut -d ' ' -f 1)" = "$digest" ]
directory=$(CDPATH='' cd -- "$(dirname "$0")" && pwd)
if ! id homewire-power >/dev/null 2>&1; then
    useradd --system --user-group --no-create-home --shell /usr/sbin/nologin homewire-power
fi
install -d -m 700 /etc/homewire-power
# Never silently replace device identity or installed authorization.
if [ -f /etc/homewire-power/server.json ]; then
    cmp -s "$config" /etc/homewire-power/server.json || { echo 'Existing configuration differs; stage an explicit rotation' >&2; exit 1; }
fi
install -m 600 "$config" /etc/homewire-power/server.json
if [ ! -f /etc/homewire-power/identity.key ]; then
    "$binary" keygen /etc/homewire-power/identity.key
fi
release=/opt/homewire-power/versions/$digest
install -d -m 755 "$release"
install -m 755 "$binary" "$release/homewire-power"
[ "$(sha256sum "$release/homewire-power" | cut -d ' ' -f 1)" = "$digest" ]
install -m 644 "$directory/homewire-power.service" /etc/systemd/system/homewire-power.service
install -m 644 "$directory/homewire-poweroff.service" /etc/systemd/system/homewire-poweroff.service
install -m 644 "$directory/50-homewire-power.rules" /etc/polkit-1/rules.d/50-homewire-power.rules
cd /opt/homewire-power
if [ -L current ]; then
    old=$(readlink current)
    if [ "$old" != "$release" ]; then
        ln -s "$old" previous.new
        mv -Tf previous.new previous
    fi
fi
ln -s "$release" current.new
mv -Tf current.new current
systemctl daemon-reload
systemctl enable homewire-power.service
systemctl restart homewire-power.service
systemctl is-active --quiet homewire-power.service
echo 'Shutdown listener installed; no shutdown requested'
