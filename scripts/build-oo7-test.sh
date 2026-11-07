#!/usr/bin/env bash
# Test-only daemon, pinned upstream source; never installs/enables a host service.
set -euo pipefail
repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
validation_target=${CARGO_TARGET_DIR:-"$repo_dir/target"}
validation_target=$(realpath -m -- "$validation_target")
revision=989e1c1ddf4fb36c98548a86c39a96afa5f25b52 # oo7 0.7.0
archive_sha=58274c0c63c4fba503d48436e353645877f1646cdb95b8a15ff8e1cb8ba78095
cache_dir="$validation_target/oo7-test-$revision"
mkdir -p "$cache_dir"
exec 9>"$cache_dir/.build.lock"
flock 9
archive="$cache_dir/upstream.tar.gz"
if [[ ! -f "$archive" ]]; then
  curl --fail --location --silent --show-error --max-time 120 \
    "https://codeload.github.com/linux-credentials/oo7/tar.gz/$revision" -o "$archive.download"
  mv "$archive.download" "$archive"
fi
printf '%s  %s\n' "$archive_sha" "$archive" | sha256sum --check --status
if [[ ! -d "$cache_dir/source" ]]; then
  mkdir "$cache_dir/source"
  tar -xzf "$archive" --strip-components=1 -C "$cache_dir/source"
fi
CARGO_TARGET_DIR="$validation_target" cargo build --locked \
  --manifest-path "$cache_dir/source/Cargo.toml" -p oo7-daemon --bin oo7-daemon
printf '%s\n' "$validation_target/debug/oo7-daemon"
