#!/usr/bin/env bash
# Detect drift between the committed WiX definition and what cargo-dist's
# template would generate today, allowing exactly the delta we applied on
# purpose.
#
# crates/otl/wix/main.wxs is hand-maintained: it needs
# `AllowSameVersionUpgrades='yes'`, which dist's template does not emit and
# no config key controls, so `allow-dirty = ["msi"]` stops dist from
# regenerating or diffing it. That buys the setting at the cost of the
# automatic check - Description, Manufacturer, path-guid, InstallerVersion
# and the list of installed binaries would all silently stop tracking the
# package. (Adding a second `[[bin]]` is the case that would actually break
# an installer.)
#
# So: regenerate into a throwaway copy of the repo *without* the exclusion,
# and diff. The only differences allowed are lines this repository added -
# the explanatory comment block and the one attribute. Anything else means
# the template moved and the hand-maintained file has to be reconciled.
#
# Reconciling: run `dist generate` in a copy with `allow-dirty` removed,
# then re-apply the comment block and AllowSameVersionUpgrades to the
# result.
#
# Needs `dist` on PATH. Skipped with a clear message when it is absent, so
# a local run without cargo-dist installed does not fail confusingly.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WXS_RELPATH="crates/otl/wix/main.wxs"

# Lines that may appear in the committed file but not in dist's output.
# Everything else must match byte for byte.
ALLOWED_MARKER="AllowSameVersionUpgrades='yes'"

if ! command -v dist >/dev/null 2>&1; then
    echo "note: cargo-dist ('dist') is not installed; skipping WiX drift check" >&2
    exit 0
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT
COPY="${WORK_DIR}/repo"

# Copy the working tree, not HEAD: a local run should check what is on disk.
# `target/` is excluded because it is large and irrelevant to `dist generate`.
mkdir -p "${COPY}"
if command -v rsync >/dev/null 2>&1; then
    rsync -a --exclude='/target' --exclude='/.git' "${REPO_ROOT}/" "${COPY}/"
else
    (cd "${REPO_ROOT}" && tar --exclude='./target' --exclude='./.git' -cf - .) |
        (cd "${COPY}" && tar -xf -)
fi

# Drop the msi exclusion so `dist generate` rewrites the wxs.
python3 - "${COPY}/dist-workspace.toml" <<'PYEOF'
import re
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as handle:
    text = handle.read()
patched, count = re.subn(r'(?m)^allow-dirty\s*=.*\n', "", text)
if count == 0:
    sys.exit("error: no `allow-dirty` key found in dist-workspace.toml; this check assumes one")
with open(path, "w", encoding="utf-8") as handle:
    handle.write(patched)
PYEOF

# `dist generate` also rewrites CI; only the wxs is of interest here, and
# regenerating both keeps dist from complaining about the other being stale.
(cd "${COPY}" && dist generate >/dev/null)

COMMITTED="${REPO_ROOT}/${WXS_RELPATH}"
REGENERATED="${COPY}/${WXS_RELPATH}"

if [[ ! -f "${REGENERATED}" ]]; then
    echo "error: dist did not regenerate ${WXS_RELPATH}; the check cannot run" >&2
    exit 1
fi

# The committed file must still carry the setting the whole exercise is for.
if ! grep -qF "${ALLOWED_MARKER}" "${COMMITTED}"; then
    echo "::error file=${WXS_RELPATH}::missing required setting ${ALLOWED_MARKER}" >&2
    exit 1
fi

python3 - "${COMMITTED}" "${REGENERATED}" "${WXS_RELPATH}" <<'PYEOF'
import re
import sys

committed_path, regenerated_path, relpath = sys.argv[1:4]

with open(committed_path, encoding="utf-8", newline="") as handle:
    committed = handle.read().splitlines()
with open(regenerated_path, encoding="utf-8", newline="") as handle:
    regenerated = handle.read().splitlines()

COMMENT_OPEN = re.compile(r"^\s*<!--")
COMMENT_CLOSE = re.compile(r"-->\s*$")


def strip_local_additions(lines):
    """Remove the hand-added comment block(s) and attribute so the rest can
    be compared verbatim against dist's output."""
    out = []
    in_comment = False
    for line in lines:
        if in_comment:
            if COMMENT_CLOSE.search(line):
                in_comment = False
            continue
        if COMMENT_OPEN.match(line) and not COMMENT_CLOSE.search(line):
            in_comment = True
            continue
        if "AllowSameVersionUpgrades" in line:
            continue
        out.append(line)
    return out


left = strip_local_additions(committed)
right = strip_local_additions(regenerated)

if left == right:
    print(f"ok: {relpath} matches dist's template apart from the documented delta")
    sys.exit(0)

import difflib

diff = list(
    difflib.unified_diff(
        right, left, fromfile="dist generate", tofile=relpath, lineterm="", n=2
    )
)
print(f"::error file={relpath}::WiX definition has drifted from cargo-dist's template", file=sys.stderr)
for line in diff[:80]:
    print(line, file=sys.stderr)
sys.exit(
    "error: main.wxs differs from dist's output beyond the documented delta "
    "(comment blocks and AllowSameVersionUpgrades). Package metadata or the "
    "binary list has probably changed; reconcile the hand-maintained file."
)
PYEOF
