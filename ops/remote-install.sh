#!/usr/bin/env bash
# Shared local transport and remote installer for setup.sh and deploy.sh.
set -euo pipefail
umask 077
mode=$1
shift
if [[ $# -ne 2 || -z $1 || $1 == -* || $1 == *[[:space:]]* || $2 != /* || $2 == *[[:cntrl:]]* ]]; then
    echo "Usage: $0 [user@]hostname /absolute/install/directory" >&2
    exit 2
fi
host=$1
install_dir=$2
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
for tool in git ssh scp mktemp; do
    command -v "$tool" >/dev/null || { echo "Required tool missing: $tool" >&2; exit 1; }
done
# Only committed build inputs and shipped examples belong in a release.
paths=(Cargo.toml Cargo.lock build.rs src migrations res ops LICENSE README.md config.json.example imprint.html.example)
revision=$(git -C "$source_dir" rev-parse --verify HEAD)
changes=$(git -C "$source_dir" status --porcelain --untracked-files=all -- "${paths[@]}" setup.sh deploy.sh)
if [[ -n $changes ]]; then
    echo 'Commit the deployment inputs first: git archive deploys HEAD, not working-tree changes.' >&2
    exit 1
fi
# Quote one argument for the remote POSIX shell, including spaces/apostrophes.
quote() { printf "'%s'" "${1//\'/\'\\\'\'}"; }
archive=$(mktemp "${TMPDIR:-/tmp}/amalthea-$mode.XXXXXXXX.tar.gz")
remote_archive=''
cleanup() {
    rm -f -- "$archive"
    if [[ -n $remote_archive ]]; then
        ssh -- "$host" "rm -f -- $(quote "$remote_archive")" || true
    fi
}
trap cleanup EXIT
git -C "$source_dir" archive --format=tar.gz --output="$archive" "$revision" -- "${paths[@]}"
printf '%s commit %s on %s:%s\n' "$mode" "$revision" "$host" "$install_dir"
remote_archive=$(ssh -- "$host" 'mktemp /tmp/amalthea-transfer.XXXXXXXX.tar.gz')
[[ $remote_archive =~ ^/tmp/amalthea-transfer\.[a-zA-Z0-9]+\.tar\.gz$ ]] || {
    echo 'Unexpected remote temporary path.' >&2
    remote_archive=''
    exit 1
}
scp -- "$archive" "$host:$remote_archive"
ssh -- "$host" "bash -s -- $(quote "$mode") $(quote "$install_dir") $(quote "$remote_archive")" <<'REMOTE'
set -euo pipefail
umask 077
mode=$1
install_dir=$2
archive=$3
for tool in cargo cc tar python3 systemctl flock; do
    command -v "$tool" >/dev/null || { echo "Required remote tool missing: $tool" >&2; exit 1; }
done
[[ $(uname -s) == Linux ]] || { echo 'Linux is required on the target.' >&2; exit 1; }
systemctl --user show-environment >/dev/null || { echo 'A running user systemd manager is required.' >&2; exit 1; }
mkdir -p -- "$install_dir"
install_dir=$(cd -- "$install_dir" && pwd -P)
[[ $install_dir != / ]] || { echo 'The install directory must not be /.' >&2; exit 1; }
unit_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user
mkdir -p -- "$unit_dir"
# One user service: serialize attempts aimed at different install directories too.
exec 9>"$unit_dir/amalthea-install.lock"
flock -n 9 || { echo 'Another setup or deployment is running.' >&2; exit 1; }
if systemctl --user cat amalthea.service >/dev/null 2>&1; then
    service_dir=$(systemctl --user show amalthea.service --property=WorkingDirectory --value)
    [[ $service_dir == "$install_dir" ]] || {
        echo "amalthea.service already belongs to another directory: $service_dir" >&2
        exit 1
    }
fi
if [[ $mode == setup ]]; then
    [[ ! -e "$install_dir/bin/amalthea" ]] || { echo 'Already installed; use deploy.sh to update it.' >&2; exit 1; }
else
    [[ -f "$install_dir/config.json" && -f "$install_dir/bin/amalthea" && -d "$install_dir/res" ]] || {
        echo 'Run setup.sh for this target first.' >&2
        exit 1
    }
    systemctl --user cat amalthea.service >/dev/null
fi
# Stage the build while the old binary and assets continue serving requests.
release=$(mktemp -d "$install_dir/.release.XXXXXXXX")
trap 'rm -rf -- "$release"' EXIT
tar -C "$release" -xzf "$archive"
[[ -f "$release/res/static/vendor/solid.js" ]] || { echo 'Static vendor assets are missing.' >&2; exit 1; }
cargo build --release --locked --manifest-path "$release/Cargo.toml" --target-dir "$install_dir/target"
install -m 0755 -- "$install_dir/target/release/amalthea" "$release/amalthea"
python3 - "$install_dir" "$release/ops/amalthea.service.in" "$release/amalthea.service" <<'PY'
import pathlib, sys
path = sys.argv[1].replace('%', '%%')
text = pathlib.Path(sys.argv[2]).read_text()
# Path directives take literal paths; ExecStart uses shell-like word parsing.
escaped = path.replace('\\', '\\\\').replace('"', '\\"').replace('$', '$$')
text = text.replace('@EXEC_INSTALL_DIR@', escaped).replace('@INSTALL_DIR@', path)
pathlib.Path(sys.argv[3]).write_text(text)
PY
mkdir -p -- "$install_dir/bin" "$install_dir/ops"
for item in config.json.example imprint.html.example LICENSE README.md; do
    install -m 0600 -- "$release/$item" "$install_dir/$item"
done
cp -R -- "$release/ops/." "$install_dir/ops/"
if [[ $mode == setup && ! -e "$install_dir/config.json" ]]; then
    install -m 0600 -- "$release/config.json.example" "$install_dir/config.json"
fi
# Save a binary/assets pair for manual recovery. Never roll back migrated data.
if [[ $mode == deploy ]]; then
    cp -p -- "$install_dir/bin/amalthea" "$install_dir/bin/amalthea.previous"
    rm -rf -- "$install_dir/res.previous"
    systemctl --user stop amalthea.service
fi
if [[ -e "$install_dir/res" ]]; then
    mv -- "$install_dir/res" "$install_dir/res.previous"
fi
mv -- "$release/res" "$install_dir/res"
mv -f -- "$release/amalthea" "$install_dir/bin/amalthea"
install -m 0600 -- "$release/amalthea.service" "$unit_dir/amalthea.service"
systemctl --user daemon-reload
if [[ $mode == deploy ]]; then
    systemctl --user start amalthea.service
    systemctl --user --no-pager status amalthea.service
else
    systemctl --user enable amalthea.service
    printf 'Installed in %s\nEdit %s/config.json on the server, then run there:\n  systemctl --user start amalthea\n' "$install_dir" "$install_dir"
    printf 'For startup at boot without a login, ask the administrator to enable lingering for %s.\n' "$(id -un)"
fi
REMOTE
