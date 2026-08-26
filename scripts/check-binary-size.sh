#!/usr/bin/env bash
# Assert the shipped `otl` binary stays inside its size budget, and fail the
# build when it does not.
#
# Size target (planning/epics.md NFR2, specs/spec-outline-cli/stack.md
# "分发"): a single static binary of roughly 5 MB. That is the ceiling we
# promise users, not a useful regression gate - by the time a change pushed
# the binary from 2.5 MB to 5 MB the damage would be long merged.
#
# So the gate is set tighter than the promise:
#
#   gate:  4 MiB = 4194304 B   (fails the build)
#   warn:  85% of the gate     (prints a warning, still passes)
#
# Measurements, `--profile dist` (= release: opt-level="s", fat LTO,
# codegen-units=1, strip="symbols", panic="abort"), aarch64-apple-darwin,
# 2026-08:
#
#   this branch, which carries release config and no feature code:
#       2_567_312 B  (~2.45 MiB)   61% of the gate
#
#   the four feature branches waiting to merge, measured individually as
#   deltas against the same baseline:
#       epic2-auth      +317_360 B
#       epic3-commands  +283_168 B
#       epic4-config    +232_800 B
#       epic4-specsync  +116_400 B
#
# Those deltas are additive-worst-case, so the merged binary lands around
# 3.35 MiB on darwin. x86_64-unknown-linux-musl runs roughly 9% larger
# because it statically links libc, which puts the largest shipped artifact
# near 3.66 MiB - about 91% of this gate.
#
# That is deliberately tight, and it is why the warning band exists: the
# squeeze becomes visible before it becomes a red build, so the response can
# be "what grew?" rather than "raise the number". 4 MiB was NOT chosen for
# comfort - it is the largest value that still leaves the gate meaningful,
# since the NFR2 promise itself (5 MB ~= 4.77 MiB) is only 14% above the
# projected merged size. A gate at the promise would police nothing.
#
# After the feature branches merge, re-measure all four targets on develop
# and update the numbers above. If the real figure is materially worse than
# the projection, the fix is to find the growth - `cargo bloat`, a duplicated
# dependency, a monomorphisation blowup - not to move the constant. Raising
# it is a deliberate decision: update the measurements here in the same
# commit and say which dependency bought the extra megabyte.
#
# Usage:
#   ./scripts/check-binary-size.sh                          # gate at 4 MiB
#   MAX_BINARY_SIZE_BYTES=3000000 ./scripts/check-binary-size.sh
#   BINARY_SIZE_WARN_PERCENT=90 ./scripts/check-binary-size.sh
#   BINARY_SIZE_PROFILE=release ./scripts/check-binary-size.sh
#   BINARY_SIZE_TARGET=x86_64-unknown-linux-musl ./scripts/check-binary-size.sh
#   SKIP_BUILD=1 ./scripts/check-binary-size.sh             # measure as-is
set -euo pipefail

# Gate: maximum size of the shipped binary, in bytes (4 MiB).
MAX_BYTES="${MAX_BINARY_SIZE_BYTES:-4194304}"

# Warning band: percentage of the gate above which the build still passes
# but says so loudly.
WARN_PERCENT="${BINARY_SIZE_WARN_PERCENT:-85}"

# Cargo profile to measure. Default `dist` is the profile cargo-dist builds
# release artifacts with, i.e. exactly what users download.
PROFILE="${BINARY_SIZE_PROFILE:-dist}"

# Rust target triple to build for. Empty means the host, which is what a
# local run and the native pre-merge runners want; the release build passes
# the triple it is publishing so the gate measures the artifact users
# actually download rather than a near-miss.
TARGET="${BINARY_SIZE_TARGET:-}"

# Binary name is fixed by the CLI contract (SPEC.md): crate `outline-cli`,
# binary `otl`.
PACKAGE="outline-cli"
BIN_NAME="otl"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Cargo puts `dev`/`test` output in target/debug and every other profile in
# target/<profile>, prefixed by the triple when --target is used.
case "${PROFILE}" in
    dev | test) PROFILE_SUBDIR="debug" ;;
    *) PROFILE_SUBDIR="${PROFILE}" ;;
esac
if [[ -n "${TARGET}" ]]; then
    OUT_DIR="${REPO_ROOT}/target/${TARGET}/${PROFILE_SUBDIR}"
else
    OUT_DIR="${REPO_ROOT}/target/${PROFILE_SUBDIR}"
fi

# Mirror the RUSTFLAGS cargo-dist appends per target environment
# (cargo-dist 0.32.0, cargo-dist/src/build/cargo.rs). Without this the
# measured binary is not the shipped binary - statically linking the CRT
# changes its size - and cargo would rebuild from scratch when `dist build`
# runs afterwards in the same job instead of reusing this compilation.
#
# Keep in sync with dist's defaults: `msvc-crt-static` defaults to true, and
# musl always gets crt-static + link-self-contained. If dist-workspace.toml
# ever sets `msvc-crt-static = false`, drop the msvc arm here too.
FLAG_TARGET="${TARGET}"
if [[ -z "${FLAG_TARGET}" ]]; then
    FLAG_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
fi
case "${FLAG_TARGET}" in
    *-msvc) DIST_RUSTFLAGS=" -Ctarget-feature=+crt-static" ;;
    *-musl) DIST_RUSTFLAGS=" -Ctarget-feature=+crt-static -Clink-self-contained=yes" ;;
    *) DIST_RUSTFLAGS="" ;;
esac

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    build_args=(--profile "${PROFILE}" -p "${PACKAGE}"
        --manifest-path "${REPO_ROOT}/Cargo.toml")
    if [[ -n "${TARGET}" ]]; then
        build_args+=(--target "${TARGET}")
    fi
    RUSTFLAGS="${RUSTFLAGS:-}${DIST_RUSTFLAGS}" cargo build "${build_args[@]}"
fi

# Windows is a first-class platform: the artifact is otl.exe there, and this
# script runs on windows runners under Git Bash. Probe both names rather
# than assuming a Unix binary.
BIN=""
for candidate in "${OUT_DIR}/${BIN_NAME}" "${OUT_DIR}/${BIN_NAME}.exe"; do
    if [[ -f "${candidate}" ]]; then
        BIN="${candidate}"
        break
    fi
done
if [[ -z "${BIN}" ]]; then
    echo "error: no ${BIN_NAME} binary in ${OUT_DIR} (looked for ${BIN_NAME} and ${BIN_NAME}.exe)" >&2
    exit 1
fi

SIZE_BYTES="$(wc -c < "${BIN}" | tr -d '[:space:]')"

# Integer arithmetic only: no bc/python dependency, and no float rounding
# deciding whether the build passes.
percent=$((SIZE_BYTES * 100 / MAX_BYTES))
printf 'binary: %s\n' "${BIN}"
printf 'size:   %s bytes (%d.%02d MiB), %s%% of the %s byte budget\n' \
    "${SIZE_BYTES}" \
    "$((SIZE_BYTES / 1048576))" \
    "$((SIZE_BYTES % 1048576 * 100 / 1048576))" \
    "${percent}" \
    "${MAX_BYTES}"

if ((SIZE_BYTES > MAX_BYTES)); then
    echo "error: ${BIN_NAME} is ${SIZE_BYTES} bytes, over the ${MAX_BYTES} byte budget by $((SIZE_BYTES - MAX_BYTES)) bytes" >&2
    echo "hint: inspect what grew with 'cargo bloat --profile ${PROFILE} -p ${PACKAGE}' or 'cargo tree -p ${PACKAGE} -e normal'" >&2
    exit 1
fi

if ((percent >= WARN_PERCENT)); then
    # Deliberately not a failure: the point is to surface the squeeze while
    # there is still room to act on it, instead of presenting a maintainer
    # with a red build and an obvious-looking constant to raise.
    warning="${BIN_NAME} is at ${percent}% of its ${MAX_BYTES} byte budget (warn at ${WARN_PERCENT}%); find what grew before the gate turns red"
    echo "::warning::${warning}"
    echo "warning: ${warning}" >&2
fi

echo "binary size gate passed"
