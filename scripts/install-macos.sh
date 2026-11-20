#!/bin/sh
# Install the compiler on macOS. The compiler and the programs it builds are Linux x86-64 only
# (docs/building.md), so this script sets up a small Ubuntu virtual machine with Lima, installs the
# compiler in it with scripts/install.sh, and puts a `weby` command on the Mac that runs the
# compiler in the VM, in the directory you are in.
#
# Usage: scripts/install-macos.sh [--yes] [--bin-dir DIR] [--cpus N] [--memory GiB] [--disk GiB]
#
# Needs Homebrew; offers to install it if it is missing. Your home directory is shared with the VM
# (writable), so `weby build app.wb -o app` writes `app` into your folder, as a Linux executable:
# run it with `weby run app.wb`, or inside the VM with `limactl shell weby ./app`. Servers
# listening in the VM are reachable on localhost on the Mac.
#
# On Apple Silicon the VM emulates x86-64, which is slow: building the compiler takes much longer
# (an hour is possible), and programs run several times slower than on real hardware. On an Intel
# Mac it is virtualised at full speed.
set -eu

assume_yes=0
bin_dir=$HOME/.local/bin
cpus=4
memory=6
disk=40
vm=weby
while [ $# -gt 0 ]; do
    case $1 in
        --yes | -y) assume_yes=1 ;;
        --bin-dir) [ $# -ge 2 ] || { echo "--bin-dir needs a directory" >&2; exit 2; }; bin_dir=$2; shift ;;
        --cpus) cpus=$2; shift ;;
        --memory) memory=$2; shift ;;
        --disk) disk=$2; shift ;;
        -h | --help) sed -n '2,/^set -eu/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
confirm() {
    [ "$assume_yes" -eq 1 ] && return 0
    printf '%s [y/N] ' "$1"
    read -r answer || answer=n
    case $answer in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

[ "$(uname -s)" = Darwin ] || die "this script is for macOS; on Linux run scripts/install.sh"
root=$(cd "$(dirname "$0")/.." && pwd)
[ -f "$root/Cargo.toml" ] && [ -f "$root/scripts/install.sh" ] || die "run this script from a checkout of the repository"

arch=x86_64
if [ "$(uname -m)" = arm64 ]; then
    say "Apple Silicon: the VM will emulate x86-64, which is slow (see --help)."
fi

say "This will: install Lima with Homebrew, create an Ubuntu 24.04 VM named '$vm' ($cpus CPUs, ${memory} GiB memory, ${disk} GiB disk), install the compiler in it, and add $bin_dir/weby."
confirm "Continue?" || die "stopped; nothing was changed"

if ! command -v brew >/dev/null 2>&1; then
    say "Homebrew is not installed."
    confirm "Install it with its official installer (https://brew.sh)?" || die "Homebrew is needed to install Lima; install it, or install Lima yourself (https://lima-vm.io)"
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    for brew in /opt/homebrew/bin/brew /usr/local/bin/brew; do
        [ -x "$brew" ] && eval "$("$brew" shellenv)" && break
    done
    command -v brew >/dev/null 2>&1 || die "Homebrew was installed but is not on the PATH; open a new terminal and run this script again"
fi

command -v limactl >/dev/null 2>&1 || { say "Installing Lima"; brew install lima; }

status=$(limactl list --format '{{.Status}}' "$vm" 2>/dev/null || true)
if [ -z "$status" ]; then
    say "Creating the VM (downloads the Ubuntu 24.04 cloud image, about 600 MB)"
    config=$(mktemp -t weby-lima).yaml
    cat >"$config" <<YAML
arch: "$arch"
cpus: $cpus
memory: "${memory}GiB"
disk: "${disk}GiB"
images:
  - location: "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img"
    arch: "x86_64"
mounts:
  - location: "~"
    writable: true
containerd:
  system: false
  user: false
YAML
    limactl start --name="$vm" --tty=false "$config"
    rm -f "$config"
elif [ "$status" != Running ]; then
    say "Starting the VM"
    limactl start "$vm"
fi

say "Copying the sources into the VM and installing the compiler (this takes a few minutes)"
tar -C "$root" --exclude=./target --exclude=./.git -cf - . |
    limactl shell "$vm" -- sh -c 'rm -rf "$HOME/weby-src" && mkdir -p "$HOME/weby-src" && tar -C "$HOME/weby-src" -xf -'
limactl shell "$vm" -- sh "\$HOME/weby-src/scripts/install.sh" --yes

say "Adding the weby command to $bin_dir"
mkdir -p "$bin_dir"
cat >"$bin_dir/weby" <<'WRAPPER'
#!/bin/sh
# Runs the compiler installed in the Lima VM, in the current directory (the Mac's home directory is
# shared with the VM at the same path). Starts the VM if it is stopped.
vm=weby
if [ "$(limactl list --format '{{.Status}}' "$vm" 2>/dev/null)" != Running ]; then
    limactl start "$vm" >/dev/null || exit 1
fi
exec limactl shell --workdir "$PWD" "$vm" -- sh -c 'exec "$HOME/.local/bin/weby" "$@"' weby "$@"
WRAPPER
chmod +x "$bin_dir/weby"

case ":$PATH:" in
    *":$bin_dir:"*) say "Done. Try: weby --version" ;;
    *) say "Done. $bin_dir is not on your PATH; add this line to ~/.zshrc, then open a new terminal:"
       echo "    export PATH=\"$bin_dir:\$PATH\"" ;;
esac
