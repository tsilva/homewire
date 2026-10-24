#!/system/bin/sh
# Configuration and transactions are root-owned; snapshot files are never sourced.
set -eu
umask 077
export PATH=/system/bin:/system/xbin
unset LD_PRELOAD LD_LIBRARY_PATH
here=${0%/*}
# shellcheck source=/dev/null
. "$here/config.sh"
: "${STATE:?}" "${SU:?}" "${PREFIX:?}" "${APP_UID:?}" "${APP_CONTEXT:?}" "${APP_USER:?}" "${GUARD:?}" "${HOMEWIRE:?}"
txn=$STATE/transaction
setting() { value=$(settings get global "$1") || return; printf '%s\n' "$value"; }
restore_setting() {
    value=$(cat "$txn/$1")
    if [ "$value" = null ]; then settings delete global "$1" >/dev/null
    else settings put global "$1" "$value"; fi
}
identity() {
    p=$1 marker=$2
    case "$p" in ''|*[!0-9]*) return 1;; esac
    [ -r "/proc/$p/cmdline" ] || return 1
    cmdline=$(tr '\000' ' ' < "/proc/$p/cmdline")
    case "$cmdline" in *"$marker"*) return 0;; *) return 1;; esac
}
wait_dead() {
    n=0
    while kill -0 "$1" 2>/dev/null; do
        [ "$(awk '{print $3}' "/proc/$1/stat")" != Z ] || return 0
        [ "$n" -lt 80 ] || return 1
        n=$((n+1)); sleep .1
    done
}
termux() {
    "$SU" -d -g "$APP_UID" -G 3003 -Z "$APP_CONTEXT" "$APP_UID" -s /system/bin/sh -c \
        "export PREFIX='$PREFIX'; export PATH='$PREFIX/bin:/system/bin'; $1"
}
leave() {
    [ -f "$txn/complete" ] || return 0
    # Keep the journal after any failure, allowing another recovery attempt.
    dumpsys deviceidle unforce >/dev/null
    for key in wifi_scan_always_enabled ble_scan_always_enabled low_power; do restore_setting "$key"; done
    if [ "$(cat "$txn/airplane_mode_on")" = 1 ]; then cmd connectivity airplane-mode enable
    else cmd connectivity airplane-mode disable; fi
    if [ "$(cat "$txn/wifi_on")" = 1 ]; then svc wifi enable; else svc wifi disable; fi
    if [ "$(cat "$txn/bluetooth_on")" = 1 ]; then svc bluetooth enable; else svc bluetooth disable; fi
    if [ "$(cat "$txn/wakelock")" = 1 ]; then
        am startservice --user "$APP_USER" -a com.termux.service_wake_lock com.termux/com.termux.app.TermuxService >/dev/null
    fi
    # Enforce existing ingress protection before restarting listeners.
    "$GUARD" check
    while IFS= read -r name; do
        if grep -Fxq "$name" "$txn/services"; then termux "sv -w 15 up '$PREFIX/var/service/$name'"; fi
    done < "$here/early-services"
    if [ "$(cat "$txn/session" 2>/dev/null || true)" = nightly ] && [ -n "$TELEGRAM_CONFIG" ]; then
        echo discard_telegram > "$STATE/action-phase"
        attempt=0
        until "$here/telegram" "$TELEGRAM_CONFIG" "$TELEGRAM_SECTION" "$PREFIX/bin/curl"; do
            attempt=$((attempt+1)); [ "$attempt" -lt 3 ] || return 1
            sleep 3
        done
        echo discarded > "$STATE/telegram-resume"
    fi
    while IFS='|' read -r pid exe born; do
        if [ "$(readlink "/proc/$pid/exe" 2>/dev/null || true)" = "$exe" ] &&
           [ "$(awk '{print $22}' "/proc/$pid/stat" 2>/dev/null || true)" = "$born" ]; then kill -CONT "$pid"; fi
    done < "$txn/frozen"
    while IFS='|' read -r pid pidfile marker launcher; do
        live=$(cat "$pidfile" 2>/dev/null || true)
        if ! identity "$live" "$marker"; then
            nohup /system/bin/sh "$launcher" </dev/null >/dev/null 2>&1 &
        fi
    done < "$txn/units"
    while IFS= read -r name; do termux "sv -w 15 up '$PREFIX/var/service/$name'"; done < "$txn/services"
    for key in airplane_mode_on wifi_on bluetooth_on; do
        [ "$(setting "$key")" = "$(cat "$txn/$key")" ] || return 1
    done
    [ "$(dumpsys deviceidle get force)" = false ] || return 1
    printf '%s\n' 'radios_restored=true services_restored=true forced_doze=false' > "$STATE/restore-proof"
    rm -rf "$txn"
}
enter() {
    [ ! -e "$txn" ] || return 1
    rm -rf "$STATE/snapshot.new"
    echo snapshot > "$STATE/action-phase"
    mkdir "$STATE/snapshot.new"
    for key in airplane_mode_on wifi_on bluetooth_on wifi_scan_always_enabled ble_scan_always_enabled low_power; do
        setting "$key" > "$STATE/snapshot.new/$key"
    done
    for key in airplane_mode_on wifi_on bluetooth_on low_power; do
        case "$(cat "$STATE/snapshot.new/$key")" in 0|1) ;; *) return 1;; esac
    done
    [ "$(dumpsys deviceidle get force)" = false ] || return 1
    dumpsys nfc | grep -q "^mState=off$" || return 1
    dumpsys power | sed -n '/Wake Locks: size=/,/Suspend Blockers:/p' > "$STATE/snapshot.new/locks"
    if grep -q 'termux:service-wakelock' "$STATE/snapshot.new/locks"; then echo 1; else echo 0; fi > "$STATE/snapshot.new/wakelock"
    : > "$STATE/snapshot.new/units"
    while IFS='|' read -r pidfile marker launcher; do
        [ -f "$pidfile" ] || continue
        pid=$(cat "$pidfile")
        [ -d "/proc/$pid" ] || continue
        identity "$pid" "$marker" || return 1
        printf '%s|%s|%s|%s\n' "$pid" "$pidfile" "$marker" "$launcher" >> "$STATE/snapshot.new/units"
    done < "$here/units"
    : > "$STATE/snapshot.new/services"
    for service in "$PREFIX"/var/service/*; do
        [ -d "$service" ] || continue
        name=${service##*/}
        case "$name" in *[!a-zA-Z0-9_-]*) return 1;; esac
        state=$(termux "sv status '$service'" || true)
        case "$state" in run:*) printf '%s\n' "$name" >> "$STATE/snapshot.new/services";; esac
    done
    : > "$STATE/snapshot.new/frozen"
    while IFS= read -r exe; do
        for proc in /proc/[0-9]*; do
            [ "$(readlink "$proc/exe" 2>/dev/null || true)" = "$exe" ] || continue
            born=$(awk '{print $22}' "$proc/stat")
            printf '%s|%s|%s\n' "${proc##*/}" "$exe" "$born" >> "$STATE/snapshot.new/frozen"
        done
    done < "$here/freeze-executables"
    cp "$STATE/session" "$STATE/snapshot.new/session"
    touch "$STATE/snapshot.new/complete"
    mv "$STATE/snapshot.new" "$txn"
    sync
    echo stop_workloads > "$STATE/action-phase"
    while IFS='|' read -r pid pidfile marker launcher; do identity "$pid" "$marker"; kill -TERM "$pid"; done < "$txn/units"
    # Homewire's root supervisor does not own the daemon's process group.
    if [ -f "$HOMEWIRE/run/homewire.pid" ]; then
        pid=$(cat "$HOMEWIRE/run/homewire.pid")
        if identity "$pid" "$HOMEWIRE/current/homewire"; then kill -TERM "$pid"; wait_dead "$pid"; fi
    fi
    while IFS='|' read -r pid pidfile marker launcher; do wait_dead "$pid"; done < "$txn/units"
    while IFS='|' read -r pid exe born; do kill -STOP "$pid"; done < "$txn/frozen"
    while IFS= read -r name; do termux "sv -w 15 down '$PREFIX/var/service/$name'"; done < "$txn/services"
    echo radios > "$STATE/action-phase"
    am startservice --user "$APP_USER" -a com.termux.service_wake_unlock com.termux/com.termux.app.TermuxService >/dev/null
    settings put global wifi_scan_always_enabled 0
    settings put global ble_scan_always_enabled 0
    settings put global low_power 1
    cmd connectivity airplane-mode enable
    svc wifi disable
    svc bluetooth disable
    echo doze > "$STATE/action-phase"
    input keyevent 223
    dumpsys deviceidle force-idle deep >/dev/null
    # Do not accept partial entry: verify actual radios, Doze and application lock.
    [ "$(setting airplane_mode_on)" = 1 ]
    [ "$(setting wifi_on)" = 0 ]
    [ "$(setting bluetooth_on)" = 0 ]
    [ "$(dumpsys deviceidle get deep)" = IDLE ]
    if dumpsys power | sed -n '/Wake Locks: size=/,/Suspend Blockers:/p' | grep -q 'termux:service-wakelock'; then return 1; fi
    echo asleep > "$STATE/action-phase"
    printf '%s\n' 'radios_off=true services_parked=true termux_wakelock=false' > "$STATE/entry-proof"
}
case "${1:-}" in
    check) value=$(setting airplane_mode_on); case "$value" in 0|1) ;; *) exit 1;; esac;;
    enter) enter;;
    leave) leave;;
    *) exit 2;;
esac
