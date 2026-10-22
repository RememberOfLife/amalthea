#!/usr/bin/env bash
# Run ON the Linux server as the service's unprivileged user. Never uses sudo.
set -euo pipefail
umask 077
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
install_dir=${1:-"$HOME/.local/share/amalthea"}
if [[ $# -gt 1 || $install_dir != /* || $install_dir == *$'\n'* ]]; then
    echo "Usage: $0 [absolute-install-directory]" >&2
    exit 2
fi
for tool in cargo cc tar python3 systemctl; do
    command -v "$tool" >/dev/null || { echo "Required tool missing: $tool" >&2; exit 1; }
done
[[ $(uname -s) == Linux ]] || { echo 'Linux is required.' >&2; exit 1; }
systemctl --user show-environment >/dev/null || { echo 'A running user systemd manager is required.' >&2; exit 1; }
[[ -f "$source_dir/res/static/vendor/solid.js" ]] || { echo 'Static vendor assets are missing.' >&2; exit 1; }
mkdir -p -- "$install_dir"
install_dir=$(cd -- "$install_dir" && pwd -P)
if [[ "$source_dir" != "$install_dir" ]]; then
    tar -C "$source_dir" -cf - Cargo.toml Cargo.lock build.rs src migrations res ops docs LICENSE README.md setup.sh deploy.sh config.json.example imprint.html.example |
        tar -C "$install_dir" -xf -
fi
cargo build --release --locked --manifest-path "$install_dir/Cargo.toml" --target-dir "$install_dir/target"
mkdir -p -- "$install_dir/bin"
install -m 0755 -- "$install_dir/target/release/amalthea" "$install_dir/bin/amalthea.new"
mv -f -- "$install_dir/bin/amalthea.new" "$install_dir/bin/amalthea"
if [[ ! -f "$install_dir/config.json" ]]; then
    install -m 0600 -- "$install_dir/config.json.example" "$install_dir/config.json"
fi
unit_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}/systemd/user
mkdir -p -- "$unit_dir"
python3 - "$install_dir" "$unit_dir/amalthea.service" <<'PY'
import pathlib, sys
root = pathlib.Path(sys.argv[1])
escaped = str(root).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
text = (root / 'ops/amalthea.service.in').read_text().replace('@INSTALL_DIR@', escaped)
pathlib.Path(sys.argv[2]).write_text(text)
PY
systemctl --user daemon-reload
systemctl --user enable amalthea.service
printf 'Installed in %s\nEdit %s/config.json, then run:\n  systemctl --user start amalthea\n' "$install_dir" "$install_dir"
printf 'To run after logout/at boot, ask the administrator to enable lingering for %s.\n' "$USER"
