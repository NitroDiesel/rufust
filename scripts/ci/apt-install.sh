#!/usr/bin/env bash
# Install apt packages on a CI runner, retrying when the mirror is unreachable.
# Usage: scripts/ci/apt-install.sh <package>...
set -euo pipefail

options=(-o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30)
for attempt in 1 2 3 4 5; do
  # `apt-get update` exits 0 when it only warns about unreachable indexes, so
  # the install decides whether the attempt worked.
  if sudo apt-get "${options[@]}" update &&
    sudo apt-get "${options[@]}" install -y "$@"; then
    exit 0
  fi
  if [ "$attempt" -lt 5 ]; then
    echo "apt attempt $attempt failed; retrying in $((attempt * 30))s" >&2
    sleep $((attempt * 30))
  fi
done
echo "apt could not install the packages after 5 attempts" >&2
exit 1
