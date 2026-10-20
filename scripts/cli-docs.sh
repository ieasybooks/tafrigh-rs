#!/bin/sh
# Run from the repository root. Usage's parser and renderers are dev-dependencies.
set -eu
spec_file=$(mktemp)
trap 'rm -f "$spec_file"' EXIT HUP INT TERM
cargo run --quiet --locked --package tafrigh-cli --bin tafrigh -- spec > "$spec_file"
cargo run --quiet --locked --package tafrigh-cli --example cli-docs -- "${1:-docs}" < "$spec_file"
