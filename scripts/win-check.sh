#!/usr/bin/env bash
# Type-check and lint the workspace for Windows, from a Unix machine.
#
# Why this exists: `std::os::unix` does not exist on Windows, and neither
# does `std::os::windows` here. Code (including test code) that reaches for
# one without a `cfg` guard compiles perfectly on the machine it was written
# on and takes the whole test harness down on the other platform. CI has a
# windows-latest leg that catches it, but only after a push.
#
# Why the dependency swap: reqwest's `rustls` feature pulls in aws-lc-sys,
# whose build script needs `windows.h` and cannot cross-compile from macOS or
# Linux. `rustls-no-provider` drops that C dependency, which is fine for a
# type check and lint - no crypto is executed. It must NEVER be committed:
# without a provider, reqwest panics at runtime for want of one. Hence the
# trap below, which restores the manifest on every exit path including Ctrl-C.
set -euo pipefail

TARGET="${1:-x86_64-pc-windows-msvc}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! rustup target list --installed | grep -qx "$TARGET"; then
    echo "installing target $TARGET" >&2
    rustup target add "$TARGET"
fi

BACKUP="$(mktemp -d)"
cp Cargo.toml Cargo.lock "$BACKUP/"
restore() {
    cp "$BACKUP/Cargo.toml" "$BACKUP/Cargo.lock" .
    rm -rf "$BACKUP"
}
trap restore EXIT INT TERM

# Swap the crypto provider for the duration of the check only.
python3 - <<'PY'
import pathlib
p = pathlib.Path("Cargo.toml")
text = p.read_text()
needle = '    "rustls",\n    "webpki-roots",\n'
if needle not in text:
    raise SystemExit("Cargo.toml no longer has the expected reqwest features; "
                     "update scripts/win-check.sh")
p.write_text(text.replace(needle, '    "rustls-no-provider",\n    "webpki-roots",\n', 1))
PY

echo "== cargo clippy --target $TARGET --workspace --all-targets -- -D warnings"
cargo clippy --target "$TARGET" --workspace --all-targets -- -D warnings
echo "OK: the workspace type-checks and lints clean for $TARGET"
