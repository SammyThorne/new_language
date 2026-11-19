#!/bin/sh
# Prepare the Zed extension for "Install Dev Extension" from this checkout. Zed builds the grammar
# by cloning a git repository at a fixed commit; the GitHub repository is private, so this copies
# the extension to a directory of its own (the first argument, or ~/.local/share/weby-zed) with the
# grammar pointed at this checkout's current commit. Commit any grammar change before running it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(git -C "$here" rev-parse --show-toplevel)
rev=$(git -C "$root" rev-parse HEAD)
dest=${1:-"$HOME/.local/share/weby-zed"}

mkdir -p "$dest"
cp -R "$here/languages" "$here/src" "$here/Cargo.toml" "$dest/"
sed -e "s|^repository = \"https://github.com/[^\"]*\"\$|repository = \"file://$root\"|" \
    -e "s|^rev = \".*\"\$|rev = \"$rev\"|" \
    "$here/extension.toml" > "$dest/extension.toml"
echo "Prepared $dest (grammar at $rev)."
echo "In Zed: Extensions > Install Dev Extension, and choose that directory."
