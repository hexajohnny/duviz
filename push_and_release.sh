#!/usr/bin/env bash
set -euo pipefail

REPO_URL="https://github.com/virtualmassacre/duviz.git"
TAG="v0.1"
ASSET="dist/duviz-linux-x86_64.zip"

cd "$(dirname "$0")"

git remote remove origin >/dev/null 2>&1 || true
git remote add origin "$REPO_URL"

git fetch origin main
git pull --rebase origin main

git push -u origin main

git tag -f "$TAG"
git push -f origin "$TAG"

if ! command -v gh >/dev/null 2>&1; then
  echo "gh CLI is required to publish the release" >&2
  echo "Install: https://cli.github.com/" >&2
  exit 1
fi

if [[ ! -f "$ASSET" ]]; then
  echo "Missing $ASSET. Build it first:" >&2
  echo "  cargo build --release" >&2
  echo "  mkdir -p dist && cp target/release/duviz dist/" >&2
  echo "  (cd dist && zip -q duviz-linux-x86_64.zip duviz)" >&2
  exit 1
fi

gh release delete "$TAG" -y || true
gh release create "$TAG" "$ASSET" --title "$TAG" --notes "Linux x86_64 binary"
