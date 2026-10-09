#!/bin/sh
# No code, services, receivers or Android process; assigns INTERNET to a private UID.
set -eu
cd "$(dirname "$0")/../../.."
: "${HOMEWIRE_ANDROID_PLATFORM:?Path to Android 35 platform directory}"
: "${HOMEWIRE_ANDROID_BUILD_TOOLS:?Path to Android build-tools}"
: "${JAVA_HOME:?Path to JDK 17}"
: "${HOMEWIRE_SIGNING_KEY:?Private persistent signing keystore path}"
: "${HOMEWIRE_SIGNING_PASSWORD_FILE:?Private keystore password file}"
export PATH="$JAVA_HOME/bin:$PATH"
umask 077
mkdir -p target/android-sandbox
"$HOMEWIRE_ANDROID_BUILD_TOOLS/aapt2" link -I "$HOMEWIRE_ANDROID_PLATFORM/android.jar" --manifest scripts/android/sandbox/AndroidManifest.xml -o target/android-sandbox/identity-unsigned.apk
"$HOMEWIRE_ANDROID_BUILD_TOOLS/zipalign" -f 4 target/android-sandbox/identity-unsigned.apk target/android-sandbox/identity-aligned.apk
"$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" sign --ks "$HOMEWIRE_SIGNING_KEY" --ks-pass "file:$HOMEWIRE_SIGNING_PASSWORD_FILE" --out target/android-sandbox/identity.apk target/android-sandbox/identity-aligned.apk
"$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" verify target/android-sandbox/identity.apk
