#!/usr/bin/env bash
# Run locally; install or update the service on the SSH target.
set -euo pipefail
source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/ops/remote-install.sh" setup "$@"
