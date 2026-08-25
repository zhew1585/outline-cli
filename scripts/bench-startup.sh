#!/usr/bin/env bash
# Measure `otl --help` cold-start time with hyperfine and fail when the mean
# exceeds the threshold. Also reports the release binary size.
#
# Performance target (specs/spec-outline-cli/SPEC.md, Constraints):
#   mean cold start < 10 ms on a local release build.
# CI runs on shared, noisy runners, so .github/workflows/ci.yml overrides the
# gate to 25 ms via STARTUP_THRESHOLD_MS; 10 ms stays the local default here.
#
# Usage:
#   ./scripts/bench-startup.sh                       # gate at 10 ms
#   STARTUP_THRESHOLD_MS=25 ./scripts/bench-startup.sh
set -euo pipefail

# Gate threshold in milliseconds (override via STARTUP_THRESHOLD_MS).
THRESHOLD_MS="${STARTUP_THRESHOLD_MS:-10}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${REPO_ROOT}/target/release/otl"
RESULTS_JSON="$(mktemp)"
trap 'rm -f "${RESULTS_JSON}"' EXIT

if ! command -v hyperfine >/dev/null 2>&1; then
    echo "error: hyperfine is not installed (apt install hyperfine / brew install hyperfine / cargo binstall hyperfine)" >&2
    exit 1
fi

cargo build --release -p outline-cli --manifest-path "${REPO_ROOT}/Cargo.toml"

SIZE_BYTES="$(wc -c < "${BIN}" | tr -d '[:space:]')"
echo "release binary: ${BIN} (${SIZE_BYTES} bytes)"

# -N (no shell) avoids shell spawn overhead skewing sub-10ms measurements.
hyperfine -N --warmup 10 --min-runs 50 --export-json "${RESULTS_JSON}" "${BIN} --help"

MEAN_MS="$(python3 - "${RESULTS_JSON}" <<'PYEOF'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)
print(f"{data['results'][0]['mean'] * 1000:.3f}")
PYEOF
)"

echo "otl --help mean startup: ${MEAN_MS} ms (threshold: ${THRESHOLD_MS} ms)"
if ! awk -v mean="${MEAN_MS}" -v max="${THRESHOLD_MS}" 'BEGIN { exit !(mean < max) }'; then
    echo "error: mean startup ${MEAN_MS} ms is not below the ${THRESHOLD_MS} ms threshold" >&2
    exit 1
fi
echo "startup gate passed"
