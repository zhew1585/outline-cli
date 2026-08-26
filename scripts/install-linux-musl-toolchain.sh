#!/usr/bin/env bash
# Install everything a Linux release build of `otl` needs to target
# x86_64-unknown-linux-musl (the statically linked artifact we ship, see
# dist-workspace.toml `targets`).
#
# Single source of truth for that package list. Both callers use it:
#   * .github/build-setup/release-build-setup.yml - the steps cargo-dist
#     injects into its own `build-local-artifacts` job, i.e. the release
#     build itself;
#   * .github/workflows/binary-size.yml - the pre-merge musl build, which is
#     what proves the release build will work before anyone tags.
# So the release path and the path that exercises it cannot drift apart.
#
# Why a C toolchain is needed at all: the TLS stack is rustls, whose crypto
# provider is aws-lc-rs -> aws-lc-sys, which compiles C. Building for a
# different libc than the host's therefore needs a musl-targeting C
# compiler. `musl-tools` provides `musl-gcc`, which the `cc` crate selects
# automatically for `*-linux-musl` targets; `cmake` drives aws-lc-sys's
# build; `libclang` backs bindgen on the fallback path where none of
# aws-lc-sys's pre-generated bindings match the target.
#
# Debian/Ubuntu only, and intended for CI. `--yes` is not optional: a
# GitHub Actions `run:` step has stdin on /dev/null, so apt's "Do you want
# to continue?" prompt would read EOF and abort.
set -euo pipefail

# Rust target triple this toolchain is for.
MUSL_TARGET="x86_64-unknown-linux-musl"

# Debian packages required to compile aws-lc-sys for a musl target.
APT_PACKAGES=(musl-tools cmake clang libclang-dev)

if ! command -v apt-get >/dev/null 2>&1; then
    echo "error: apt-get not found; this script targets Debian/Ubuntu CI runners" >&2
    exit 1
fi

export DEBIAN_FRONTEND=noninteractive
sudo apt-get update
sudo apt-get install --yes --no-install-recommends "${APT_PACKAGES[@]}"

# Fail loudly here rather than inside a confusing linker error later.
musl-gcc --version >/dev/null
echo "musl-gcc: $(command -v musl-gcc)"

# Idempotent: cargo-dist also adds the target itself, and re-adding an
# installed target is a no-op.
if command -v rustup >/dev/null 2>&1; then
    rustup target add "${MUSL_TARGET}"
else
    echo "note: rustup not on PATH; assuming ${MUSL_TARGET} std is already present" >&2
fi
