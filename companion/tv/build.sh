#!/bin/sh
# Platform tools only; the permanent signing identity must survive APK updates.
set -eu
cd "$(dirname "$0")"
: "${HOMEWIRE_ANDROID_PLATFORM:?Path to Android 35 platform}"
: "${HOMEWIRE_ANDROID_BUILD_TOOLS:?Path to Android build-tools 35.0.0}"
: "${JAVA_HOME:?Path to JDK 17}"
export PATH="$JAVA_HOME/bin:$PATH"
umask 077
: "${HOMEWIRE_TV_VERSION:?Version name supplied by build.py}"
: "${HOMEWIRE_TV_VERSION_CODE:?Version code supplied by build.py}"
: "${HOMEWIRE_TV_BUILD_DIR:?Fresh absolute build directory supplied by build.py}"
out=$HOMEWIRE_TV_BUILD_DIR
mkdir -p "$out/classes" "$out/dex"
find src -name '*.java' | LC_ALL=C sort > "$out/sources"
javac --release 8 -classpath "$HOMEWIRE_ANDROID_PLATFORM/android.jar" -d "$out/classes" @"$out/sources"
jar cf "$out/classes.jar" -C "$out/classes" .
"$HOMEWIRE_ANDROID_BUILD_TOOLS/d8" --min-api 31 --lib "$HOMEWIRE_ANDROID_PLATFORM/android.jar" --output "$out/dex" "$out/classes.jar"
"$HOMEWIRE_ANDROID_BUILD_TOOLS/aapt2" compile --dir res -o "$out/resources.zip"
"$HOMEWIRE_ANDROID_BUILD_TOOLS/aapt2" link -I "$HOMEWIRE_ANDROID_PLATFORM/android.jar" --manifest AndroidManifest.xml --replace-version --version-code "$HOMEWIRE_TV_VERSION_CODE" --version-name "$HOMEWIRE_TV_VERSION" -o "$out/unsigned.apk" "$out/resources.zip"
(cd "$out/dex" && zip -q ../unsigned.apk classes.dex)
"$HOMEWIRE_ANDROID_BUILD_TOOLS/zipalign" -f 4 "$out/unsigned.apk" "$out/aligned.apk"
if [ "${HOMEWIRE_TV_UNSIGNED:-0}" = 1 ]; then
    cp "$out/aligned.apk" "$out/homewire-tv-unsigned.apk"
else
    : "${HOMEWIRE_SIGNING_KEY:?Private persistent signing keystore path}"
    : "${HOMEWIRE_SIGNING_PASSWORD_SOURCE:?Apksigner env: password variable reference}"
    "$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" sign --ks "$HOMEWIRE_SIGNING_KEY" --ks-type PKCS12 --ks-pass "$HOMEWIRE_SIGNING_PASSWORD_SOURCE" --out "$out/homewire-tv.apk" "$out/aligned.apk"
    "$HOMEWIRE_ANDROID_BUILD_TOOLS/apksigner" verify "$out/homewire-tv.apk"
fi
