#!/usr/bin/env bash
# Assert the shipped `otl` binary stays inside its size budget, and fail the
# build when it does not.
#
# ---------------------------------------------------------------------------
# Two different numbers, because one number cannot do both jobs
# ---------------------------------------------------------------------------
# The distribution promise is a single static binary of roughly 5 MB. That is
# the promise to users, and it is a useless regression gate: by the time a
# change pushed the binary from 3.4 MB to 5 MB the damage would be long
# merged. So there are two checks:
#
#   * a per-target REGRESSION BUDGET, ~8% above what that target measures
#     today. Tight enough that one fat dependency fails the build, loose
#     enough that a toolchain bump does not.
#   * the SIZE CEILING of 5,000,000 bytes, applied to every target. This is
#     the promise, not a regression signal.
#
# ---------------------------------------------------------------------------
# Why the budget is per target
# ---------------------------------------------------------------------------
# It used to be one 4 MiB number for every published triple, calibrated on
# aarch64-apple-darwin. CI then measured x86_64-unknown-linux-musl at
# 4,523,120 B - 107% - and the gate went red on a build that had not
# regressed at all. A single number is simultaneously too loose for the
# smallest target and wrong for the largest, so budgets are per target.
#
# Only macOS ships today, so both budgets below are measured on
# this machine and nothing is extrapolated. The former musl and msvc rows are
# gone with their targets. What is worth keeping from that episode, because
# whoever re-adds a platform will need it, is that targets are NOT
# interchangeable and the differences are inherent rather than trimmable
# (measured 2026-08, `--profile dist`, dist's own RUSTFLAGS):
#
#   architecture, platform held fixed (x86_64 vs aarch64):
#       on darwin      +15.7%      on linux-musl  +20.9%
#   platform, architecture held fixed:
#       aarch64  linux-musl vs darwin            +9.0%
#       x86_64   musl vs darwin                 +13.9%
#       x86_64   musl vs windows-msvc           +20.4%
#
# Neither term dominates; they compound. Two counter-intuitive results worth
# remembering: statically linked musl was *smaller* than dynamic glibc at
# fixed architecture (3,740,216 vs 3,806,048), and Windows was the smallest
# of the three x86_64 targets despite also linking its CRT statically - so
# "static linking" was never the axis. Re-adding a target means measuring it,
# not deriving it from these.
#
# `cargo bloat` found nothing anomalous then and the dependency graph has not
# changed shape since: .text was 2.4 MiB spread as otl 414K, std 363K,
# aws-lc-sys 264K, rustls 249K, reqwest 162K, clap 124K.
#
# Fat LTO is load-bearing: `[profile.dist]` inherits it from `[profile.release]`
# rather than taking dist's default `lto = "thin"`, which measured +723,344 B
# on darwin and +525,432 B on musl.
#
# ---------------------------------------------------------------------------
# Measurements behind each budget
# ---------------------------------------------------------------------------
# `--profile dist` (= release: opt-level="s", fat LTO, codegen-units=1,
# strip="symbols", panic="abort"), 2026-08:
#
#   target                        measured     budget    of budget
#   aarch64-apple-darwin         3,782,224  4,080,000        93%
#   x86_64-apple-darwin          4,369,768  4,720,000        93%
#
# Both built locally - the x86_64 one cross-compiled on an arm64 host, which
# works because Apple's toolchain targets both slices. Every shipped target is
# therefore measurable without CI, which was not true while musl shipped: that
# figure came only from a CI leg, and the extrapolation standing in for it
# locally was wrong by 0.74 MB. No number here is an extrapolation.
#
# The two figures are the LARGER of the local and CI measurements, which
# differ slightly (aarch64: 3,782,224 local vs 3,781,984 CI; x86_64: 4,363,264
# local vs 4,369,768 CI, a 0.15% toolchain difference). CI is what gates, so
# budgeting off the smaller number would put the gate a hair under what it
# measures.
#
# Budgets sit ~8% above measurement, which is why both read the same 93% - the
# gate is equally strict on each, the point of splitting it.
#
# ---------------------------------------------------------------------------
# Why this table was 284 KB stale, and what to do about it
# ---------------------------------------------------------------------------
# The previous rows read 3,464,608 / 4,007,712 (both 92%). By 2026-08-27
# `develop` measured 3,748,832 / 4,328,752 - 99% of budget on BOTH targets,
# ~1.2 KB of headroom each. The gate had been warning at 99% ("find what grew
# before the gate turns red") on every develop run and no re-measure followed,
# so the next commit of any size was going to turn it red. That commit was the
# section-editing feature, which added 33 KB.
#
# The warning band worked; acting on it is the part that did not happen. If you
# see 95%+ in a build log, re-measure and update this table THEN - the numbers
# here are only as good as the last time someone did.
#
# HEADROOM AGAINST THE PROMISE, stated plainly because it is the number a
# future maintainer needs and it should not have to be recomputed:
#
#   x86_64-apple-darwin is the largest artifact we ship, at 4,369,768 B.
#   The promise is ~5 MB = 5,000,000 B.
#   Remaining headroom: 630,232 B (12.6%).
#
# That room is shrinking and the trend is worth naming rather than rediscovering:
# it was 992,288 B (19.8%) at the previous measurement, so ~362 KB of it went in
# one stretch of development. Note also that this target's BUDGET (4,720,000) is
# now only 280,000 B under the size ceiling - the ~8% regression margin has
# nearly caught up with the promise, and once it passes, the ceiling rather than
# the budget becomes the binding gate. Before raising this row again, consider
# whether the growth should be paid back instead.
#
# For contrast, so nobody carries over the old caution about a target we no
# longer ship: while x86_64-unknown-linux-musl shipped it was the binding target
# at 4,556,160 B, leaving ~443,840 B (8.9%). If Linux or Windows returns, the
# binding target and this number change with it - re-measure rather than
# assuming this figure still holds.
#
# Raising any budget is a deliberate act: update the measurement in the table
# in the same commit, and say which dependency bought the space.
#
# Usage:
#   ./scripts/check-binary-size.sh                          # host target
#   BINARY_SIZE_TARGET=x86_64-apple-darwin ./scripts/check-binary-size.sh
#   MAX_BINARY_SIZE_BYTES=3000000 ./scripts/check-binary-size.sh   # override
#   BINARY_SIZE_WARN_PERCENT=90 ./scripts/check-binary-size.sh
#   BINARY_SIZE_PROFILE=release ./scripts/check-binary-size.sh
#   SKIP_BUILD=1 ./scripts/check-binary-size.sh             # measure as-is
set -euo pipefail

# The size promise, in bytes. Applies to every target, overridable only to
# make the check stricter in an experiment - never relax it here.
SIZE_CEILING_BYTES="${SIZE_CEILING_BYTES:-5000000}"

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
        aarch64-apple-darwin) echo 4080000 ;;
        x86_64-apple-darwin) echo 4720000 ;;
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

ceiling_percent=$((SIZE_BYTES * 100 / SIZE_CEILING_BYTES))
printf 'cap:    %s%% of the %s byte promise\n' "${ceiling_percent}" "${SIZE_CEILING_BYTES}"

failed=0

if [[ -z "${BUDGET}" ]]; then
    # An unpublished triple - somebody's local host, or a new target added
    # without a measurement. Enforce the promise, and say plainly that the
    # regression budget is absent rather than inventing one.
    echo "note: no regression budget is defined for ${EFFECTIVE_TARGET}; enforcing only the size ceiling" >&2
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
# budget edit can quietly authorise shipping more than the promise allows.
if ((SIZE_BYTES > SIZE_CEILING_BYTES)); then
    echo "error: ${BIN_NAME} for ${EFFECTIVE_TARGET} is ${SIZE_BYTES} bytes, past the ${SIZE_CEILING_BYTES} byte size promise" >&2
    echo "hint: this is a product decision, not a threshold to edit - the promise is ~5 MB" >&2
    failed=1
elif ((ceiling_percent >= 95)); then
    warning="${BIN_NAME} for ${EFFECTIVE_TARGET} is at ${ceiling_percent}% of the ~5 MB size promise"
    echo "::warning::${warning}"
    echo "warning: ${warning}" >&2
fi

if ((failed != 0)); then
    exit 1
fi
echo "binary size gate passed"
