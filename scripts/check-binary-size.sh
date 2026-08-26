#!/usr/bin/env bash
# Assert the shipped `otl` binary stays inside its size budget, and fail the
# build when it does not.
#
# ---------------------------------------------------------------------------
# Two different numbers, because one number cannot do both jobs
# ---------------------------------------------------------------------------
# NFR2 (planning/epics.md, specs/spec-outline-cli/stack.md) promises a single
# static binary of roughly 5 MB. That is the promise to users, and it is a
# useless regression gate: by the time a change pushed the binary from 3.4 MB
# to 5 MB the damage would be long merged. So there are two checks:
#
#   * a per-target REGRESSION BUDGET, ~8% above what that target measures
#     today. Tight enough that one fat dependency fails the build, loose
#     enough that a toolchain bump does not.
#   * the NFR2 CEILING of 5,000,000 bytes, applied to every target. This is
#     the promise, not a regression signal.
#
# ---------------------------------------------------------------------------
# Why the budget is per target
# ---------------------------------------------------------------------------
# It used to be one 4 MiB number for all four published triples, calibrated on
# aarch64-apple-darwin. CI then measured x86_64-unknown-linux-musl at
# 4,523,120 B - 107% - and the gate went red on a build that had not
# regressed at all. A single number is simultaneously too loose for the
# smallest target and wrong for the largest.
#
# The 1.09 MB gap between those two triples was measured, not guessed. Three
# controlled comparisons (2026-08, `--profile dist`, dist's own RUSTFLAGS):
#
#   musl vs glibc, arch held fixed (aarch64-linux):
#       musl static 3,740,216 vs gnu dynamic 3,806,048  ->  musl is 65,832 B
#       SMALLER. Statically linking musl libc is NOT what costs the megabyte;
#       the obvious hypothesis is simply wrong.
#   Linux/ELF/static vs macOS/Mach-O, arch held fixed (aarch64):
#       3,740,216 vs 3,431,568                          ->  +308,648 B (+9.0%)
#   x86_64 vs aarch64, OS and libc held fixed (linux-musl):
#       4,523,120 vs 3,740,216                          ->  +782,904 B (+20.9%)
#   x86_64 vs aarch64 on darwin, as an independent check of the same effect:
#       3,970,784 vs 3,431,568                          ->  +539,216 B (+15.7%)
#
# So the dominant term is x86_64 code generation, not musl: the same source
# compiles ~16-21% larger for x86_64 than for aarch64 on both platforms. That
# is a property of the instruction set, and no amount of dependency trimming
# changes it. `cargo bloat` on the musl build agrees that nothing is
# anomalous - .text is 2.4 MiB spread as otl 414K, std 363K, aws-lc-sys 264K,
# rustls 249K, reqwest 162K, clap 124K, with no single unexpected entry.
#
# Fat LTO is confirmed in effect on the musl path too (cargo passes a bare
# `-C lto`); building musl with dist's default thin LTO instead costs
# +525,432 B, so that fix earns more here than on darwin.
#
# ---------------------------------------------------------------------------
# Measurements behind each budget
# ---------------------------------------------------------------------------
# `--profile dist` (= release: opt-level="s", fat LTO, codegen-units=1,
# strip="symbols", panic="abort"), 2026-08, all five tracks merged, plus the
# +33,040 B that Story 4.3 (`otl doctor`) adds once it lands:
#
#   target                        measured    +doctor     budget    of budget
#   aarch64-apple-darwin         3,431,568  3,464,608  3,750,000        92%
#   x86_64-apple-darwin          3,970,784  4,003,824  4,330,000        92%
#   x86_64-unknown-linux-musl    4,523,120  4,556,160  4,920,000        92%
#   x86_64-pc-windows-msvc        not yet measured     4,330,000  provisional
#
# (The doctor delta is the +33,040 B its implementer measured on darwin; on
# x86_64 it will scale up with the same ~1.16-1.21 factor as everything else,
# so read the musl row's +doctor column as a floor. The 8% headroom absorbs
# either value.)
#
# Sources: darwin triples built locally (the x86_64 one cross-compiled on an
# arm64 host); musl from CI; the aarch64-linux numbers above from a container,
# used for the controlled comparisons rather than as budgets, since neither
# aarch64-linux triple is published.
#
# The Windows budget is provisional because MSVC cannot be linked off Windows.
# It is not looser than reality: CI's Windows leg passed the previous 4 MiB
# gate, so that target is known to be under 4,194,304 B today. Replace this
# with the measured value from a Windows run and tighten.
#
# Every budget sits ~8% above its measurement, which is why all three measured
# targets read the same 92% - the gate is equally strict everywhere, which is
# the whole point of splitting it.
#
# The musl budget is also the binding one against NFR2: at 4,523,120 B that
# artifact is already 90% of the 5,000,000 B promise. Its budget is set below
# the ceiling on purpose, so the regression check can never authorise breaking
# the promise. If musl needs to grow past ~4.9 MB, that is a product decision
# about NFR2, not a constant to bump here.
#
# Raising any budget is a deliberate act: update the measurement in the table
# in the same commit, and say which dependency bought the space.
#
# Usage:
#   ./scripts/check-binary-size.sh                          # host target
#   BINARY_SIZE_TARGET=x86_64-unknown-linux-musl ./scripts/check-binary-size.sh
#   MAX_BINARY_SIZE_BYTES=3000000 ./scripts/check-binary-size.sh   # override
#   BINARY_SIZE_WARN_PERCENT=90 ./scripts/check-binary-size.sh
#   BINARY_SIZE_PROFILE=release ./scripts/check-binary-size.sh
#   SKIP_BUILD=1 ./scripts/check-binary-size.sh             # measure as-is
set -euo pipefail

# The NFR2 promise, in bytes. Applies to every target, overridable only to
# make the check stricter in an experiment - never relax it here.
NFR2_CEILING_BYTES="${NFR2_CEILING_BYTES:-5000000}"

# Percentage of a budget at which the build still passes but says so loudly.
# 95 keeps the band a narrow "you are about to break it" strip rather than a
# permanent warning nobody reads.
WARN_PERCENT="${BINARY_SIZE_WARN_PERCENT:-95}"

# Cargo profile to measure. Default `dist` is the profile cargo-dist builds
# release artifacts with, i.e. exactly what users download.
PROFILE="${BINARY_SIZE_PROFILE:-dist}"

# Rust target triple to build for. Empty means the host; the release build and
# the pre-merge matrix both name the triple they publish, so the gate measures
# the artifact users actually download rather than a near-miss.
TARGET="${BINARY_SIZE_TARGET:-}"

# Binary name is fixed by the CLI contract (SPEC.md): crate `outline-cli`,
# binary `otl`.
PACKAGE="outline-cli"
BIN_NAME="otl"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Resolve the host triple once: needed both to pick a budget and to decide
# which RUSTFLAGS dist would apply.
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
EFFECTIVE_TARGET="${TARGET:-${HOST_TRIPLE}}"

# Per-target regression budgets. See the table above for the measurement each
# one is derived from. A published target missing from here is a bug, not a
# reason to fall back to something arbitrary - scripts/check-release-gating.sh
# asserts that every triple in dist-workspace.toml appears in this case.
budget_for_target() {
    case "$1" in
        aarch64-apple-darwin) echo 3750000 ;;
        x86_64-apple-darwin) echo 4330000 ;;
        x86_64-unknown-linux-musl) echo 4920000 ;;
        x86_64-pc-windows-msvc) echo 4330000 ;;
        *) echo "" ;;
    esac
}

BUDGET="${MAX_BINARY_SIZE_BYTES:-$(budget_for_target "${EFFECTIVE_TARGET}")}"

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
case "${EFFECTIVE_TARGET}" in
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
printf 'binary: %s\n' "${BIN}"
printf 'target: %s\n' "${EFFECTIVE_TARGET}"
printf 'size:   %s bytes (%d.%02d MiB)\n' \
    "${SIZE_BYTES}" \
    "$((SIZE_BYTES / 1048576))" \
    "$((SIZE_BYTES % 1048576 * 100 / 1048576))"

nfr2_percent=$((SIZE_BYTES * 100 / NFR2_CEILING_BYTES))
printf 'NFR2:   %s%% of the %s byte promise\n' "${nfr2_percent}" "${NFR2_CEILING_BYTES}"

failed=0

if [[ -z "${BUDGET}" ]]; then
    # An unpublished triple - somebody's local host, or a new target added
    # without a measurement. Enforce the promise, and say plainly that the
    # regression budget is absent rather than inventing one.
    echo "note: no regression budget is defined for ${EFFECTIVE_TARGET}; enforcing only the NFR2 ceiling" >&2
    echo "      (add a measured row to budget_for_target() before publishing this target)" >&2
else
    percent=$((SIZE_BYTES * 100 / BUDGET))
    printf 'budget: %s%% of the %s byte budget for this target\n' "${percent}" "${BUDGET}"
    if ((SIZE_BYTES > BUDGET)); then
        echo "error: ${BIN_NAME} for ${EFFECTIVE_TARGET} is ${SIZE_BYTES} bytes, over its ${BUDGET} byte budget by $((SIZE_BYTES - BUDGET)) bytes" >&2
        echo "hint: inspect what grew with 'cargo bloat --profile ${PROFILE} -p ${PACKAGE} --target ${EFFECTIVE_TARGET}'" >&2
        echo "hint: budgets are per target and derived from measurements; if this target legitimately" >&2
        echo "      grew, re-measure and update the table in this script's header in the same commit" >&2
        failed=1
    elif ((percent >= WARN_PERCENT)); then
        # Deliberately not a failure: surface the squeeze while there is still
        # room to act, instead of presenting a maintainer with a red build and
        # an obvious-looking constant to raise.
        warning="${BIN_NAME} for ${EFFECTIVE_TARGET} is at ${percent}% of its ${BUDGET} byte budget (warn at ${WARN_PERCENT}%); find what grew before the gate turns red"
        echo "::warning::${warning}"
        echo "warning: ${warning}" >&2
    fi
fi

# The promise is checked independently of the regression budget, so no future
# budget edit can quietly authorise shipping more than NFR2 allows.
if ((SIZE_BYTES > NFR2_CEILING_BYTES)); then
    echo "error: ${BIN_NAME} for ${EFFECTIVE_TARGET} is ${SIZE_BYTES} bytes, past the ${NFR2_CEILING_BYTES} byte NFR2 promise" >&2
    echo "hint: this is a product decision, not a threshold to edit - NFR2 says ~5 MB" >&2
    failed=1
elif ((nfr2_percent >= 95)); then
    warning="${BIN_NAME} for ${EFFECTIVE_TARGET} is at ${nfr2_percent}% of the NFR2 ~5 MB promise"
    echo "::warning::${warning}"
    echo "warning: ${warning}" >&2
fi

if ((failed != 0)); then
    exit 1
fi
echo "binary size gate passed"
