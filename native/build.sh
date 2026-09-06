#!/bin/sh
# Build the native core in release mode and place it where `basedpython_ui._native` imports it:
#   src/basedpython_ui/_native<EXT_SUFFIX>   (gitignored via *.so)
# The interpreter is python3.14 (PYO3_PYTHON overrides it; .cargo/config.toml sets the default).
set -eu
cd "$(dirname "$0")"
PY="${PYO3_PYTHON:-python3.14}"
export PYO3_PYTHON="$PY"
cargo build --release
suffix="$("$PY" -c 'import sysconfig; print(sysconfig.get_config_var("EXT_SUFFIX"))')"
dest="../src/basedpython_ui/_native${suffix}"
if [ -f target/release/lib_native.dylib ]; then
    built=target/release/lib_native.dylib
elif [ -f target/release/lib_native.so ]; then
    built=target/release/lib_native.so
else
    echo "build.sh: no lib_native artefact in target/release" >&2
    exit 1
fi
# install under a fresh inode: overwriting a module in place while a running app has it
# mapped makes macOS kill every later process that maps the file (code signature invalid)
cp "$built" "$dest.new"
mv -f "$dest.new" "$dest"
echo "built $dest"
