#!/system/bin/sh
set -eu
umask 077
release=$1 state=$2 mode=$3
shift 3
exec "$release/controller" --supervise "$state" "$release/action.sh" "$mode" "$@"
