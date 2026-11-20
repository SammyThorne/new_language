#!/bin/sh
# Install the compiler on Debian, Ubuntu and other apt-based Linux systems, starting from nothing:
# the script installs LLVM 19, the Boehm collector, a C toolchain and Rust if they are missing,
# builds the compiler and runtime from this checkout, and installs the `weby` command.
#
# Usage: scripts/install.sh [--prefix DIR] [--yes] [--dry-run] [--no-system-packages]
#
#   --prefix DIR            where `weby` goes: DIR/bin/weby and DIR/lib/weby (default ~/.local)
#   --yes                   do not ask before installing system packages
#   --dry-run               print what would be done, and change nothing
#   --no-system-packages    skip apt (and sudo); build with what is already installed
#
# Windows and macOS: see install.ps1 and install-macos.sh, which set up a Linux environment and
# run this script in it. Other Linux distributions: docs/building.md lists what to install.
set -eu

prefix=${PREFIX:-$HOME/.local}
assume_yes=0
dry_run=0
system_packages=1
while [ $# -gt 0 ]; do
    case $1 in
        --prefix) [ $# -ge 2 ] || { echo "--prefix needs a directory" >&2; exit 2; }; prefix=$2; shift ;;
        --prefix=*) prefix=${1#--prefix=} ;;
        --yes | -y) assume_yes=1 ;;
        --dry-run) dry_run=1 ;;
        --no-system-packages) system_packages=0 ;;
        -h | --help) sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

root=$(cd "$(dirname "$0")/.." && pwd)
llvm_prefix=/usr/lib/llvm-19

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
# Run a command that changes the system, or only print it with --dry-run.
run() {
    printf '+ %s\n' "$*"
    [ "$dry_run" -eq 1 ] || "$@"
}

# What this script can work on.
[ "$(uname -s)" = Linux ] || die "this script is for Linux; on macOS run scripts/install-macos.sh, on Windows scripts/install.ps1"
[ "$(uname -m)" = x86_64 ] || die "the compiler supports x86-64 only (docs/building.md); this machine is $(uname -m)"
[ -f "$root/Cargo.toml" ] && [ -f "$root/Makefile" ] || die "run this script from a checkout of the repository: $root is not one"

if [ "$system_packages" -eq 1 ]; then
    command -v apt-get >/dev/null 2>&1 || die "no apt-get: this script supports Debian and Ubuntu; for other distributions follow docs/building.md, or pass --no-system-packages once the packages are installed"
    [ -r /etc/os-release ] || die "no /etc/os-release"
    # shellcheck disable=SC1091
    . /etc/os-release
    codename=${VERSION_CODENAME:-}
    if [ "$(id -u)" -eq 0 ]; then
        sudo=""
    elif command -v sudo >/dev/null 2>&1; then
        sudo="sudo"
    elif [ "$dry_run" -eq 1 ]; then
        sudo="sudo"
    else
        die "not running as root and no sudo; run as root, or install the packages yourself and pass --no-system-packages"
    fi

    # Build tools, git, the Boehm collector (programs link libgc.so.1), and what LLVM's shared
    # library needs to be linked against.
    packages="build-essential make curl ca-certificates git gnupg binutils libgc1 zlib1g-dev libzstd-dev"

    say "This will install system packages with apt: $packages, and LLVM 19 (llvm-19-dev, libpolly-19-dev)."
    if [ "$assume_yes" -ne 1 ] && [ "$dry_run" -ne 1 ]; then
        printf 'Continue? [y/N] '
        read -r answer || answer=n
        case $answer in y | Y | yes | YES) ;; *) die "stopped; nothing was installed" ;; esac
    fi

    run $sudo apt-get update
    # shellcheck disable=SC2086
    run $sudo apt-get install -y $packages

    # LLVM 19 exactly. Debian 13 and Ubuntu 25.04 and later have it; older releases get it from
    # apt.llvm.org.
    candidate=$(apt-cache policy llvm-19-dev 2>/dev/null | sed -n 's/^ *Candidate: *//p')
    if [ -z "$candidate" ] || [ "$candidate" = "(none)" ]; then
        [ -n "$codename" ] || die "cannot tell which release this is, so cannot add apt.llvm.org; install llvm-19-dev yourself (docs/building.md)"
        say "llvm-19-dev is not in this release's archive; adding apt.llvm.org for $codename"
        run $sudo install -d -m 755 /etc/apt/keyrings
        if [ "$dry_run" -eq 1 ]; then
            echo "+ curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | $sudo tee /etc/apt/keyrings/apt.llvm.org.asc"
            echo "+ write /etc/apt/sources.list.d/llvm-19.list"
        else
            curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key | $sudo tee /etc/apt/keyrings/apt.llvm.org.asc >/dev/null
            echo "deb [signed-by=/etc/apt/keyrings/apt.llvm.org.asc] https://apt.llvm.org/$codename/ llvm-toolchain-$codename-19 main" |
                $sudo tee /etc/apt/sources.list.d/llvm-19.list >/dev/null
        fi
        run $sudo apt-get update
    fi
    run $sudo apt-get install -y llvm-19-dev libpolly-19-dev
fi

# Rust 1.85 or newer, from rustup when there is none.
rust_ok() {
    command -v rustc >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1 || return 1
    v=$(rustc --version | cut -d' ' -f2)
    major=${v%%.*}
    rest=${v#*.}
    minor=${rest%%.*}
    [ "$major" -gt 1 ] || [ "$minor" -ge 85 ]
}
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! rust_ok; then
    if command -v rustup >/dev/null 2>&1; then
        say "Updating Rust with rustup"
        run rustup update stable
    elif command -v rustc >/dev/null 2>&1; then
        die "rustc $(rustc --version | cut -d' ' -f2) is older than 1.85 and was not installed by rustup; remove it or update it, then run this script again"
    else
        say "Installing Rust with rustup (https://rustup.rs)"
        if [ "$dry_run" -eq 1 ]; then
            echo "+ curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal"
        else
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
        fi
    fi
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
fi

cd "$root"
export LLVM_SYS_191_PREFIX="${LLVM_SYS_191_PREFIX:-$llvm_prefix}"

if [ "$dry_run" -eq 1 ]; then
    say "Would run: make check-deps, then make install PREFIX=$prefix (in $root)"
    exit 0
fi

say "Checking the requirements"
sh scripts/check-deps.sh

say "Building the compiler and runtime, and installing into $prefix (this takes a few minutes)"
make install PREFIX="$prefix"

say "Done. Try:  $prefix/bin/weby --version"
