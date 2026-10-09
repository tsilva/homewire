#!/bin/sh
set -eu
cd "$(dirname "$0")/../../.."
: "${HOMEWIRE_NDK:?Set HOMEWIRE_NDK to Android NDK 30.0.16248370}"
case $(uname -s) in Darwin) host=darwin-x86_64 ;; Linux) host=linux-x86_64 ;; *) exit 1 ;; esac
cc="$HOMEWIRE_NDK/toolchains/llvm/prebuilt/$host/bin/aarch64-linux-android28-clang"
mkdir -p target/android-sandbox
for binary in enter probe log; do
  "$cc" -D_GNU_SOURCE -Wall -Wextra -Werror -Os -fPIE -pie -Wl,-z,relro,-z,now -s \
    "scripts/android/sandbox/$binary.c" -o "target/android-sandbox/$binary"
done
"$cc" -D_GNU_SOURCE -Wall -Wextra -Werror -Os -fPIC -shared -Wl,-z,relro,-z,now -s \
  scripts/android/sandbox/guard.c -o target/android-sandbox/guard.so
