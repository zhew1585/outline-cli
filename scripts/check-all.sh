#!/usr/bin/env bash
# Run every local gate and report each one's REAL exit status.
#
# This exists because of a mistake worth not repeating: checking a gate with
#
#     cargo fmt --all -- --check 2>&1 | tail -1 && echo "PASS"
#
# tests `tail`'s exit status, not the gate's. `tail` always succeeds, so that
# line prints PASS for a failing gate - and it did, for several pushes, until
# CI disagreed on all three platforms at once. Anything that summarises a
# gate must capture its status before piping.
#
# Usage: bash scripts/check-all.sh [--windows] [--linux]
#   --windows  also cross-check for x86_64-pc-windows-msvc (needs the target)
#   --linux    also run the suite on real Linux in docker (needs docker)
set -uo pipefail

# `|| exit` matters here for the same reason as the header above: without it a
# failed cd would run every gate against whatever directory we happened to be
# in and report the results as if they were this repository's.
cd "$(dirname "$0")/.." || exit 1
export PATH="$HOME/.cargo/bin:$PATH"

WITH_WINDOWS=0
WITH_LINUX=0
for arg in "$@"; do
  case "$arg" in
    --windows) WITH_WINDOWS=1 ;;
    --linux) WITH_LINUX=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

FAILED=()
run() {
  local name="$1"; shift
  local log
  log="$(mktemp)"
  if "$@" >"$log" 2>&1; then
    printf '  ok   %s\n' "$name"
  else
    printf '  FAIL %s\n' "$name"
    FAILED+=("$name")
    sed 's/^/         /' "$log" | tail -25
  fi
  rm -f "$log"
}

echo "local gates:"
run "cargo fmt --check" cargo fmt --all -- --check
run "cargo clippy -D warnings" cargo clippy --workspace --all-targets -- -D warnings
run "cargo test --workspace" cargo test --workspace
run "cargo doc" cargo doc --workspace --no-deps
run "binary size" bash scripts/check-binary-size.sh

if [ "$WITH_WINDOWS" = 1 ]; then
  echo "windows cross-check:"
  run "win-check.sh" bash scripts/win-check.sh
fi

if [ "$WITH_LINUX" = 1 ]; then
  echo "real linux (docker):"
  # The target dir is a host mount, not a path inside the container: the
  # container's own filesystem is small enough that a debug build of the
  # whole workspace fills it and the link step fails with ENOSPC, which
  # reads like a build error and is not one.
  #
  # --user matters for correctness, not tidiness. Containers run as root by
  # default, and root has CAP_DAC_OVERRIDE: it ignores permission bits. Any
  # test that asserts a permission is ENFORCED therefore gets the wrong
  # answer - `a_write_into_an_unwritable_directory_reports_failure` writes
  # into a 0500 directory and expects a failure, and as root the write
  # succeeds, so the test fails here while being correct everywhere else.
  # Running as the invoking user is what makes this gate agree with CI.
  #
  # CARGO_HOME has to move with it: the image's is root-owned, so a non-root
  # cargo cannot write the registry index into it and the build dies before
  # any test runs.
  mkdir -p target/linux target/linux-cargo
  run "linux cargo test" docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$PWD":/w -v "$PWD/target/linux":/linux-target \
    -v "$PWD/target/linux-cargo":/linux-cargo -w /w \
    -e CARGO_TARGET_DIR=/linux-target -e CARGO_HOME=/linux-cargo \
    rust:1.98-slim \
    cargo test --workspace
fi

echo
if [ ${#FAILED[@]} -eq 0 ]; then
  echo "all gates passed"
  exit 0
fi
printf 'FAILED: %s\n' "${FAILED[*]}"
exit 1
