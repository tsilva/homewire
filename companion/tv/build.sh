#!/bin/sh
# Platform tools only; the permanent signing identity must survive APK updates.
set -eu
cd "$(dirname "$0")"
: "${HOMEWIRE_ANDROID_PLATFORM:?Path to Android 35 platform}"
: "${HOMEWIRE_ANDROID_BUILD_TOOLS:?Path to Android build-tools 35.0.0}"
: "${JAVA_HOME:?Path to JDK 17}"
: "${HOMEWIRE_SIGNING_KEY:?Private persistent signing keystore path}"
: "${HOMEWIRE_SIGNING_PASSWORD_SOURCE:?Apksigner env: password variable reference}"
export PATH="$JAVA_HOME/bin:$PATH"
umask 077
rm -rf build/classes build/dex
mkdir -p build/classes build/dex
find src -name '*.java' > build/sources
javac --release 8 -classpath "$HOMEWIRE_ANDROID_PLATFORM/android.jar" -d build/classes @build/sources
jar cf build/classes.jar -C build/classes .
"$HOMEWIRE_ANDROID_BUILD_TOOLS/d8" --min-api 31 --lib "$HOMEWIRE_ANDROID_PLATFORM/android.jar" --output build/dex build/classes.jar
"$HOMEWIRE_ANDROID_BUILD_TOOLS/aapt2" link -I "$HOMEWIRE_ANDROID_PLATFORM/android.jar" --manifest AndroidManifest.xml -o build/unsigned.apk
(cd build/dex && zip -q -u ../unsigned.apk classes.dex)
"$HOMEWIRE_ANDROID_BUILD_TOOLS/zipalign" -f 4 build/unsigned.apk build/aligned.apk
"$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" sign --ks "$HOMEWIRE_SIGNING_KEY" --ks-type PKCS12 --ks-pass "$HOMEWIRE_SIGNING_PASSWORD_SOURCE" --out build/homewire-tv.apk build/aligned.apk
"$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" verify build/homewire-tv.apk
