#!/bin/sh
# Check that what is needed to build the compiler, and to link the programs it compiles, is
# installed. Prints one line per requirement and exits with status 1 if a required one is missing.
# The packages are listed in docs/building.md.
#
# Usage: scripts/check-deps.sh   (or `make check-deps`)
set -u
root=$(cd "$(dirname "$0")/.." && pwd)
missing=0

found() { printf '  ok       %-20s %s\n' "$1" "$2"; }
absent() {
    printf '  MISSING  %-20s %s\n' "$1" "$2"
    missing=1
}
optional() { printf '  -        %-20s %s\n' "$1" "$2"; }

echo "Required"

system="$(uname -s) $(uname -m)"
if [ "$system" = "Linux x86_64" ]; then
    found "Linux on x86-64" "$system"
else
    absent "Linux on x86-64" "this is $system; the runtime's task switching and the code generator only support Linux on x86-64"
fi

if command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    version=$(rustc --version | cut -d' ' -f2)
    major=${version%%.*}
    rest=${version#*.}
    minor=${rest%%.*}
    if [ "$major" -gt 1 ] || [ "$minor" -ge 85 ]; then
        found "Rust 1.85 or newer" "rustc $version"
    else
        absent "Rust 1.85 or newer" "rustc $version is too old; run \`rustup update\`"
    fi
else
    absent "Rust 1.85 or newer" "no rustc and cargo on PATH; install them from https://rustup.rs"
fi

prefix=${LLVM_SYS_191_PREFIX:-$(sed -n 's/^LLVM_SYS_191_PREFIX *= *"\(.*\)"/\1/p' "$root/.cargo/config.toml")}
config="$prefix/bin/llvm-config"
if [ -x "$config" ] && "$config" --version | grep -q '^19\.'; then
    if [ -n "$(ls "$("$config" --libdir)"/libLLVM-19*.so* 2>/dev/null)" ]; then
        found "LLVM 19" "$("$config" --version) in $prefix"
    else
        absent "LLVM 19" "$prefix has no shared libLLVM-19; install llvm-19-dev"
    fi
else
    absent "LLVM 19" "no LLVM 19 llvm-config in $prefix; install llvm-19-dev, or set LLVM_SYS_191_PREFIX"
fi

cc=${CC:-cc}
if command -v "$cc" >/dev/null 2>&1; then
    found "C compiler" "$(command -v "$cc")"
    gc=$("$cc" -print-file-name=libgc.so.1 2>/dev/null)
    if [ -n "$gc" ] && [ "$gc" != "libgc.so.1" ] && [ -e "$gc" ]; then
        found "Boehm GC" "$gc"
    elif /sbin/ldconfig -p 2>/dev/null | grep -q 'libgc\.so\.1 '; then
        found "Boehm GC" "libgc.so.1 (ldconfig)"
    else
        absent "Boehm GC" "no libgc.so.1; install libgc1"
    fi
else
    absent "C compiler" "no \`$cc\`; install gcc (or clang), or set CC"
    absent "Boehm GC" "not checked without a C compiler; install libgc1"
fi

echo
echo "Optional"

if command -v git >/dev/null 2>&1; then
    found "git" "for \`fetch\` of git dependencies"
else
    optional "git" "not found; needed only to fetch git dependencies"
fi
if command -v python3 >/dev/null 2>&1; then
    found "Python 3" "for the benchmarks"
else
    optional "Python 3" "not found; needed only for the benchmarks"
fi
go=$(command -v go 2>/dev/null || { [ -x "$HOME/.local/go/bin/go" ] && echo "$HOME/.local/go/bin/go"; })
if [ -n "$go" ]; then
    found "Go" "for the benchmark comparisons ($go)"
else
    optional "Go" "not found; needed only for the benchmark comparisons"
fi
if command -v psql >/dev/null 2>&1 || command -v pg_ctl >/dev/null 2>&1 || ls /usr/lib/postgresql/*/bin/postgres >/dev/null 2>&1; then
    found "PostgreSQL" "for the database test"
else
    optional "PostgreSQL" "not found; needed only for the database test"
fi
if /sbin/ldconfig -p 2>/dev/null | grep -q 'libsqlite3\.so'; then
    found "SQLite" "libsqlite3, for programs that import sqlite"
else
    optional "SQLite" "no libsqlite3; needed only by programs that import sqlite and by its tests (libsqlite3-0)"
fi

echo
if [ "$missing" -ne 0 ]; then
    echo "Something required is missing; see docs/building.md."
    exit 1
fi
echo "Everything required is installed."
