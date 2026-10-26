#!/usr/bin/env bash
# Downloads the model files named in each models/*/model.toml and checks
# their SHA-256. Model weights are not kept in git. Works in Git Bash too.
set -euo pipefail
cd "$(dirname "$0")/../models"
for manifest in */model.toml; do
  dir=$(dirname "$manifest")
  file=$(sed -n 's/^file *= *"\(.*\)"/\1/p' "$manifest")
  url=$(sed -n 's/^url *= *"\(.*\)"/\1/p' "$manifest")
  sum=$(sed -n 's/^sha256 *= *"\(.*\)"/\1/p' "$manifest")
  if [ ! -f "$dir/$file" ]; then
    echo "fetching $dir/$file"
    curl -sSfL -o "$dir/$file.part" "$url"
    mv "$dir/$file.part" "$dir/$file"
  fi
  if [ -n "$sum" ]; then
    echo "$sum  $dir/$file" | sha256sum -c --quiet -
  fi
done
echo "models ready"
