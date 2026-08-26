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
#   measured 2026-08, `--profile dist` (= release: opt-level="s", fat LTO,
#   codegen-units=1, strip="symbols", panic="abort"):
#       aarch64-apple-darwin      2_567_312 B  (~2.45 MiB)
#   the other release targets land in the same 2.6-2.8 MB band, with
#   x86_64-unknown-linux-musl the largest because it statically links libc.
#
#   gate: 4 MiB = 4194304 B
#
# 4 MiB leaves ~40% headroom over the largest observed artifact - enough
# that ordinary code growth and toolchain churn never flap the build - while
# still catching the thing this gate exists to catch: a fat new dependency
# (a YAML parser, a TUI stack, an async runtime) landing in the shipped
# binary. It is also comfortably under the 5 MB NFR, so passing this gate
# implies satisfying NFR2.
#
# Raising the ceiling is a deliberate decision, not a mechanical fix:
# update the measurements above together with the constant, and say in the
# commit message which dependency bought the extra megabyte.
#
# Usage:
#   ./scripts/check-binary-size.sh                          # gate at 4 MiB
#   MAX_BINARY_SIZE_BYTES=3000000 ./scripts/check-binary-size.sh
#   BINARY_SIZE_PROFILE=release ./scripts/check-binary-size.sh
#   BINARY_SIZE_TARGET=x86_64-unknown-linux-musl ./scripts/check-binary-size.sh
#   SKIP_BUILD=1 ./scripts/check-binary-size.sh             # measure as-is
set -euo pipefail

# Gate: maximum size of the shipped binary, in bytes (4 MiB).
MAX_BYTES="${MAX_BINARY_SIZE_BYTES:-4194304}"

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

echo "binary size gate passed"
