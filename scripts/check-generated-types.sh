#!/usr/bin/env bash
#
# Fail if the checked-in TypeScript types differ from what the Rust types
# generate right now.
#
# The client never hand-writes an API type. `ts-rs` derives them from the Rust
# structs during `cargo test`, and the output is committed so the diff is
# visible in review. This script is what makes that a guarantee rather than a
# convention: an API change that breaks the client breaks the build here,
# instead of breaking production later.
#
# Run with: scripts/check-generated-types.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

generated=clients/shared/src/types

if [[ ! -d $generated ]]; then
  echo "error: $generated does not exist. Run 'cargo test' first." >&2
  exit 1
fi

before=$(mktemp -d)
trap 'rm -rf "$before"' EXIT
cp -R "$generated/." "$before/"

# ts-rs writes its exports as a side effect of the test run.
cargo test --workspace >/dev/null 2>&1

if diff -r -q "$before" "$generated" >/dev/null 2>&1; then
  echo "generated types are up to date"
  exit 0
fi

echo "error: checked-in TypeScript types are stale." >&2
echo "       the Rust types have changed and the client has not been regenerated." >&2
echo >&2
diff -r -u "$before" "$generated" >&2 || true
echo >&2
echo "commit the regenerated files under $generated" >&2
exit 1
