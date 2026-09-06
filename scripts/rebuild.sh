#!/bin/sh
# build the native core, transpile the project, and leave `out/` runnable
#
#   scripts/rebuild.sh            # BY=by, or set it to the fork's binary
#   BY=/path/to/by scripts/rebuild.sh
#
# the last step exists because `by build` copies the native module into `out/` in place:
# macOS kills any process that loads a code file whose bytes changed under it, so the
# module is re-installed under a fresh inode and `out/` can be imported again
set -eu
cd "$(dirname "$0")/.."
BY="${BY:-by}"
native/build.sh
"$BY" build --soundness none
for so in src/basedpython_ui/_native*.so; do
    [ -f "$so" ] || continue
    [ -d out/basedpython_ui ] || continue
    dest="out/basedpython_ui/$(basename "$so")"
    cp "$so" "$dest.new"
    mv -f "$dest.new" "$dest"
done
echo "out/ is up to date"
