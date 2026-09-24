#!/bin/bash
# Check artifact NAME of this workflow run, as download-artifact left it in
# directory NAME, against the .sha256 file it carries, and download it again
# if it did not arrive intact. download-artifact has reported success with
# its files missing or truncated, and gh run download starts over on every
# dropped connection (three in a row for a 9 GB disk on macOS), so this
# resumes a dropped download instead.
#
# Usage: test/fetch-artifact.sh NAME
# Needs GH_TOKEN, GITHUB_REPOSITORY and GITHUB_RUN_ID.
set -euo pipefail

name=$1
api=repos/$GITHUB_REPOSITORY/actions
zip=$name.zip
trap 'rm -f "$zip"' EXIT
sum=sha256sum
command -v sha256sum >/dev/null || sum="shasum -a 256" # macOS

verify() { (cd "$name" 2>/dev/null && $sum -c ./*.sha256); }

download() {
  local id url have=0 last stuck=0
  id=$(gh api "$api/runs/$GITHUB_RUN_ID/artifacts?name=$name" --jq '.artifacts[0].id') &&
    [[ $id =~ ^[0-9]+$ ]] || return 1
  # Gives up after 10 drops in a row that made no progress
  while [ "$stuck" -lt 10 ]; do
    # The API redirects to a URL signed for 10 minutes, so get a fresh one
    # each time. Under 100 kB/s for a minute counts as dropped.
    url=$(curl -fsS --connect-timeout 30 -o /dev/null -w '%{redirect_url}' \
      -H "Authorization: Bearer $GH_TOKEN" "https://api.github.com/$api/artifacts/$id/zip") &&
      curl -fsS --connect-timeout 30 --speed-limit 102400 --speed-time 60 \
        -C - -o "$zip" "$url" && return 0
    last=$have
    have=$(($(wc -c 2>/dev/null <"$zip" || echo 0)))
    if [ "$have" -gt "$last" ]; then stuck=0; else stuck=$((stuck + 1)); fi
    echo "$name: download dropped at $have bytes, resuming"
    sleep 5
  done
  return 1
}

extract() {
  mkdir -p "$name"
  if command -v unzip >/dev/null; then
    unzip -q "$zip" -d "$name"
  else
    7z x -y -bd -o"$name" "$zip" >/dev/null # Windows Git Bash may lack unzip
  fi
}

for i in 1 2 3; do
  verify && exit 0
  echo "attempt $i: $name did not arrive intact, downloading it again"
  rm -rf "$name" "$zip"
  download && extract || true
done
verify
