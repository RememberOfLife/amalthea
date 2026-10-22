#!/usr/bin/env bash
# Run on the development machine. Builds Rust on the Linux target, then restarts its user service.
set -euo pipefail
umask 077
if [[ $# -ne 2 || $1 == -* || $2 != /* || $1 == *$'\n'* || $2 == *$'\n'* ]]; then
    echo "Usage: $0 [user@]hostname /absolute/install/directory" >&2
    exit 2
fi
host=$1
install_dir=$2
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
for tool in ssh scp tar; do
    command -v "$tool" >/dev/null || { echo "Required tool missing: $tool" >&2; exit 1; }
done
# Quote one argument for the remote POSIX shell.
quote() { printf "'%s'" "${1//\'/\'\\\'\'}"; }
archive=$(mktemp "${TMPDIR:-/tmp}/amalthea-deploy.XXXXXXXX.tar.gz")
remote_archive=''
cleanup() {
    rm -f -- "$archive"
    if [[ -n $remote_archive ]]; then ssh -- "$host" "rm -f -- $(quote "$remote_archive")" || true; fi
}
trap cleanup EXIT
# Explicit allowlist: config, data, git metadata and local reference material are never deployed.
tar -C "$source_dir" -czf "$archive" Cargo.toml Cargo.lock build.rs src migrations res ops docs LICENSE README.md setup.sh deploy.sh config.json.example
remote_archive=$(ssh -- "$host" 'mktemp /tmp/amalthea-deploy.XXXXXXXX.tar.gz')
[[ $remote_archive =~ ^/tmp/amalthea-deploy\.[a-zA-Z0-9]+\.tar\.gz$ ]] || { echo 'Unexpected remote temporary path.' >&2; remote_archive=''; exit 1; }
scp -- "$archive" "$host:$remote_archive"
ssh -- "$host" "bash -s -- $(quote "$install_dir") $(quote "$remote_archive")" <<'REMOTE'
set -euo pipefail
umask 077
install_dir=$1
archive=$2
for tool in cargo cc tar systemctl; do
    command -v "$tool" >/dev/null || { echo "Required remote tool missing: $tool" >&2; exit 1; }
done
[[ -f "$install_dir/config.json" && -f "$install_dir/bin/amalthea" ]] || { echo 'Run setup.sh on this target first.' >&2; exit 1; }
systemctl --user cat amalthea.service >/dev/null
# Stage a complete release. The running service keeps its old binary and assets during the build.
release=$(mktemp -d "$install_dir/release.XXXXXXXX")
trap 'rm -rf -- "$release"' EXIT
tar -C "$release" -xzf "$archive"
cargo build --release --locked --manifest-path "$release/Cargo.toml" --target-dir "$install_dir/target"
install -m 0755 -- "$install_dir/target/release/amalthea" "$release/amalthea"
systemctl --user stop amalthea.service
# Keep a previous binary/assets pair for manual rollback. Database migrations are forward-only.
if [[ -d "$install_dir/res.previous" ]]; then rm -rf -- "$install_dir/res.previous"; fi
mv -- "$install_dir/res" "$install_dir/res.previous"
cp -p -- "$install_dir/bin/amalthea" "$install_dir/bin/amalthea.previous"
mv -- "$release/res" "$install_dir/res"
mv -f -- "$release/amalthea" "$install_dir/bin/amalthea"
for item in Cargo.toml Cargo.lock build.rs src migrations ops docs LICENSE README.md setup.sh deploy.sh config.json.example; do
    cp -R -- "$release/$item" "$install_dir/"
done
systemctl --user start amalthea.service
systemctl --user --no-pager status amalthea.service
REMOTE
