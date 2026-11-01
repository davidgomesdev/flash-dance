#!/bin/sh
# Download a flash-dance release for this machine, extract only the web server
# (binary + site/), and run it. Needs `gh` logged in.
#   scripts/run-web.sh [tag]        default: the latest release
# LEPTOS_SITE_ADDR=0.0.0.0:3000 to serve to other machines.
set -eu

repo=davidgomesdev/flash-dance
tag=${1:-$(gh release view --repo "$repo" --json tagName --jq .tagName)}

if [ "$(uname -s)" = Darwin ]; then
  label=aarch64-apple-darwin
else
  # Newest build the system glibc can load: 2.39 build needs glibc >= 2.39.
  glibc=$(ldd --version | head -1 | awk '{print $NF}')
  if [ "$(printf '2.39\n%s\n' "$glibc" | sort -V | head -1)" = 2.39 ]; then
    label=aarch64-linux-glibc2.39
  else
    label=aarch64-linux-glibc2.35
  fi
fi

dir=${FLASH_DANCE_DIR:-$HOME/.cache/flash-dance}/$tag-$label
if [ ! -x "$dir/bin/flash-dance-web" ]; then
  echo "downloading $tag ($label)" >&2
  mkdir -p "$dir"
  # GNU tar needs --wildcards for globs; bsdtar (macOS) treats them as patterns already.
  w=; tar --version 2>/dev/null | grep -q GNU && w=--wildcards
  gh release download "$tag" --repo "$repo" --pattern "*-$label.tar.gz" --output - |
    tar xz $w -C "$dir" --strip-components=1 '*/bin/flash-dance-web' '*/site'
fi

cd "$dir"
export LEPTOS_OUTPUT_NAME=flash-dance-web LEPTOS_SITE_ROOT=site LEPTOS_SITE_PKG_DIR=pkg
exec ./bin/flash-dance-web
