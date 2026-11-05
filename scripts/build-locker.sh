#!/usr/bin/env bash
# Pinned upstream swaylock + rendering-only patch. PAM/storage sources are untouched.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
out_dir=$1
cache_dir=$2
mkdir -p "$out_dir" "$cache_dir"
exec 9>"$cache_dir/.build.lock"
flock 9
revision=44b82de635c3bc66b0093abd1cf8cc1c8b1b9c0f # upstream v1.8.6
archive_sha=33d7fff30ab8ceb3d0fd2f16284ea918fcb9db3438508218d4c3aab2ce3fbdb6
patch_key=$(sha256sum patches/swaylock-eight-slices.patch | cut -d ' ' -f 1)
archive="$cache_dir/upstream.tar.gz"
if [[ ! -f "$archive" ]]; then
  curl --fail --location --silent --show-error \
    "https://codeload.github.com/swaywm/swaylock/tar.gz/$revision" -o "$archive.download"
  mv "$archive.download" "$archive"
fi
printf '%s  %s\n' "$archive_sha" "$archive" | sha256sum --check --status
source_dir="$cache_dir/src-$patch_key"
if [[ ! -d "$source_dir" ]]; then
  mkdir -p "$source_dir"
  tar -xzf "$archive" --strip-components=1 -C "$source_dir"
  if ! patch --batch --directory "$source_dir" -p1 < patches/swaylock-eight-slices.patch; then
    rm -rf -- "$source_dir"
    exit 1
  fi
fi
build_dir="$cache_dir/build-$patch_key"
if [[ ! -f "$build_dir/build.ninja" ]]; then
  GIT_CEILING_DIRECTORIES="$cache_dir" meson setup "$build_dir" "$source_dir" --buildtype=release --prefix=/usr --sysconfdir=/etc \
    -Dpam=enabled -Dgdk-pixbuf=disabled -Dman-pages=disabled
fi
ninja -C "$build_dir" -j4
cc -std=c11 -Wall -Wextra -Werror -I "$source_dir/include" tests/lock-ring.c -o "$build_dir/ring-test"
"$build_dir/ring-test"
install -m755 "$build_dir/swaylock" "$out_dir/.navi-swaylock.$$.tmp"
mv -f -- "$out_dir/.navi-swaylock.$$.tmp" "$out_dir/navi-swaylock"
