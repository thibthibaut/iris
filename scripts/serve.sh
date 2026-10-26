#!/bin/bash
# Runs the app on :3000 from a copy of the release binary, so `cargo build` doesn't
# clobber the running one. Typst comes from PATH (devenv) or Homebrew.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p .iris-cache
cp target/release/iris .iris-cache/iris-app
PATH="/opt/homebrew/bin:$PATH" RUST_LOG=iris=info exec .iris-cache/iris-app serve --port "${PORT:-3000}"
