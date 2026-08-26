#!/usr/bin/env bash
# Reject versions that the Windows MSI cannot express, before a release is
# built rather than in the middle of one.
#
# Windows Installer's ProductVersion is `major.minor.build[.extra]` with
# major<=255, minor<=255, build<=65534 - and, crucially, **only the first
# three fields take part in version comparison**
# (https://learn.microsoft.com/en-us/windows/win32/msi/productversion).
#
# cargo-wix (the library cargo-dist drives, 0.3.9 src/create.rs::version)
# squeezes a SemVer prerelease into that fourth, ignored field. It parses a
# number out of the prerelease heuristically - `1.2.3-4`, `1.2.3-rc.4`, or
# `1.2.3-rc+4` - and **hard errors** if it cannot, with the MSI build failing
# on the Windows runner partway through a release.
#
# Two consequences this script guards:
#
#   1. A prerelease with no numeric component (`1.2.3-alpha`, `1.2.3-beta`)
#      makes cargo-wix fail. Caught here, at plan time, with a message that
#      says what to name the tag instead.
#   2. Field bounds. Out-of-range major/minor/patch/prerelease numbers are
#      cargo-wix errors too.
#
# It does NOT fix the deeper limitation: because the fourth field is ignored
# in comparisons, 1.2.3-rc.1, 1.2.3-rc.2 and 1.2.3 all compare equal as MSI
# product versions. That is handled in crates/otl/wix/main.wxs with
# `AllowSameVersionUpgrades='yes'`, which makes the newer MSI replace the
# older one instead of installing beside it or refusing.
#
# Usage:
#   ./scripts/check-msi-version.sh            # version from crates/otl/Cargo.toml
#   ./scripts/check-msi-version.sh 1.2.3-rc.4
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# These checks are implemented in Python to stay readable; keep the
# dependency explicit and fail with a message that says what is missing
# rather than a bare "command not found" from three lines down. Every
# runner this is wired into (ubuntu-*, macos-*) ships python3; a slim
# container image may not.
require_python3() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo "error: python3 is required by $(basename "${BASH_SOURCE[1]}") but is not on PATH" >&2
        echo "hint: this gate fails closed on purpose - install python3 rather than skipping it" >&2
        exit 1
    fi
}
require_python3
MANIFEST="${REPO_ROOT}/crates/otl/Cargo.toml"

if [[ $# -ge 1 ]]; then
    VERSION="$1"
else
    # First `version = "..."` in the manifest is the [package] one.
    VERSION="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "${MANIFEST}" | head -1)"
fi
if [[ -z "${VERSION}" ]]; then
    echo "error: could not determine a version to check (manifest: ${MANIFEST})" >&2
    exit 1
fi

# A leading `v` is how the release tag spells it; accept either form.
VERSION="${VERSION#v}"

python3 - "${VERSION}" <<'PYEOF'
import re
import sys

# Windows Installer ProductVersion field bounds.
MAX_MAJOR = 255
MAX_MINOR = 255
MAX_PATCH = 65534
MAX_PRERELEASE = 65534

SEMVER = re.compile(
    r"^(?P<major>0|[1-9]\d*)"
    r"\.(?P<minor>0|[1-9]\d*)"
    r"\.(?P<patch>0|[1-9]\d*)"
    r"(?:-(?P<pre>[0-9A-Za-z.-]+))?"
    r"(?:\+(?P<build>[0-9A-Za-z.-]+))?$"
)

version = sys.argv[1]
match = SEMVER.match(version)
if not match:
    sys.exit(f"error: {version!r} is not a SemVer version cargo/dist will accept")

major = int(match.group("major"))
minor = int(match.group("minor"))
patch = int(match.group("patch"))
pre = match.group("pre") or ""
build = match.group("build") or ""

problems = []
for name, value, limit in (
    ("major", major, MAX_MAJOR),
    ("minor", minor, MAX_MINOR),
    ("patch", patch, MAX_PATCH),
):
    if value > limit:
        problems.append(f"{name} version {value} exceeds the MSI limit of {limit}")


def prerelease_number(pre: str, build: str):
    """Mirror cargo-wix 0.3.9 create.rs::version: `1.2.3-4`, then
    `1.2.3-rc.4`, then `1.2.3-rc+4`."""
    for candidate in (pre, pre.split(".", 1)[1] if "." in pre else None, build):
        if candidate is None or candidate == "":
            continue
        try:
            return int(candidate)
        except ValueError:
            continue
    return None


summary = f"{version}: MSI ProductVersion {major}.{minor}.{patch}"
if pre or build:
    bonus = prerelease_number(pre, build)
    if bonus is None:
        problems.append(
            f"the prerelease/build metadata in {version!r} has no numeric component, so "
            f"cargo-wix cannot map it onto the MSI's fourth version field and the Windows "
            f"build will fail. Name the tag like 1.2.3-rc.4 (or 1.2.3-4) instead"
        )
    elif bonus > MAX_PRERELEASE:
        problems.append(
            f"prerelease number {bonus} exceeds the MSI limit of {MAX_PRERELEASE}"
        )
    else:
        summary += (
            f".{bonus} (the .{bonus} field is ignored when Windows compares versions; "
            f"AllowSameVersionUpgrades in wix/main.wxs covers that)"
        )

if problems:
    for problem in problems:
        print(f"error: {problem}", file=sys.stderr)
    sys.exit(1)

print(summary)
PYEOF

echo "msi version gate passed"
