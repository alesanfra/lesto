#!/usr/bin/env sh
# Build the tutorial into docs/book (gitignored). Needs `cargo install mdbook`.
set -eu
cd "$(dirname "$0")"
mdbook build .
echo "built: $(pwd)/book/index.html"
