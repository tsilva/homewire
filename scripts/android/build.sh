#!/bin/sh
# Build from the repo root with Rust 1.95.0 + NDK 30.0.16248370 already installed.
set -eu
cd "$(dirname "$0")/../.."
: "${HOMEWIRE_NDK:?Set HOMEWIRE_NDK to Android NDK 30.0.16248370}"
ndk_version=$(sed -n 's/^Pkg.Revision = //p' "$HOMEWIRE_NDK/source.properties")
[ "$ndk_version" = 30.0.16248370 ] || { echo "Expected NDK 30.0.16248370" >&2; exit 1; }
case $(uname -s) in Darwin) host=darwin-x86_64 ;; Linux) host=linux-x86_64 ;; *) echo 'Use macOS or Linux' >&2; exit 1 ;; esac
ndk_bin="$HOMEWIRE_NDK/toolchains/llvm/prebuilt/$host/bin"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android28-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
[ -x "$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER" ]
rustup run 1.95.0 cargo build --release --locked --target aarch64-linux-android
mkdir -p target/android-package
cp target/aarch64-linux-android/release/homewire target/android-package/homewire
python3 - <<'PY'
import hashlib, json, subprocess
from pathlib import Path
p=Path('target/android-package/homewire')
source=hashlib.sha256()
for f in sorted([f for f in Path('src').rglob('*') if f.is_file()] + [Path('Cargo.toml'), Path('Cargo.lock')]):
    source.update(str(f).encode()+b'\0'+f.read_bytes()+b'\0')
s={'target':'aarch64-linux-android','api':28,'rust':'1.95.0','ndk':'30.0.16248370',
   'source_commit':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
   'source_dirty':bool(subprocess.check_output(['git','status','--porcelain'])),
   'source_tree_sha256':source.hexdigest(),
   'sha256':hashlib.sha256(p.read_bytes()).hexdigest()}
Path('target/android-package/manifest.json').write_text(json.dumps(s,indent=2)+'\n')
print('Android package: target/android-package')
PY
