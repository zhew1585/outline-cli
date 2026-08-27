#!/usr/bin/env bash
# Assert every third-party GitHub Action used by this repository is pinned to
# an immutable 40-character commit SHA, not a movable tag.
#
# The hand-written workflows do this by convention. The *generated* release
# workflow only does it because `[dist.github-action-commits]` in
# dist-workspace.toml tells cargo-dist which commits to emit - and a dist
# upgrade that starts using a new action would quietly reintroduce a floating
# tag like `actions/checkout@v6`. Since `dist generate --check` happily
# accepts whatever the template produces, the property has to be asserted
# directly. This matters most in the release workflow, whose jobs hold a
# writable GITHUB_TOKEN and upload the artifacts users install.
#
# Local reusable workflows (`uses: ./.github/workflows/x.yml`) are this
# repository's own code at the same commit and are exempt.
#
# Scans the whole `.github` tree, both `.yml` and `.yaml` (GitHub accepts
# either), so composite actions under `.github/actions/` and the build-setup
# fragment that dist splices into the release workflow are covered too - a
# floating tag added to that fragment would otherwise only become visible
# after someone remembered to re-run `dist generate`.
#
# Usage:
#   ./scripts/check-action-pins.sh                 # everything under .github
#   ./scripts/check-action-pins.sh path/to/wf.yml  # specific files
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

if [[ $# -gt 0 ]]; then
    FILES=("$@")
else
    FILES=()
    while IFS= read -r file; do
        FILES+=("${file}")
    done < <(find "${REPO_ROOT}/.github" -type f \( -name '*.yml' -o -name '*.yaml' \) | sort)
fi

if [[ ${#FILES[@]} -eq 0 ]]; then
    echo "error: no workflow files to check" >&2
    exit 1
fi

python3 - "${FILES[@]}" <<'PYEOF'
import re
import sys

# `- uses: owner/repo@ref` / `uses: owner/repo/path@ref`, with the ref
# optionally quoted. Comments after the ref are stripped by the pattern.
USES = re.compile(r"^\s*(?:-\s*)?uses:\s*[\"']?([^\"'#\s]+)[\"']?")
PINNED = re.compile(r"^[^@]+@[0-9a-f]{40}$")

problems = 0
checked = 0
for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as handle:
        for lineno, line in enumerate(handle, start=1):
            match = USES.match(line)
            if not match:
                continue
            ref = match.group(1)
            # A local reusable workflow, not a third-party action.
            if ref.startswith("./"):
                continue
            checked += 1
            if not PINNED.match(ref):
                problems += 1
                print(
                    f"::error file={path},line={lineno}::action '{ref}' is not pinned "
                    f"to a 40-character commit SHA",
                    file=sys.stderr,
                )
                print(f"{path}:{lineno}: NOT PINNED: {ref}", file=sys.stderr)
            else:
                print(f"{path}:{lineno}: ok: {ref}")

if checked == 0:
    sys.exit("error: found no `uses:` entries at all; the parser is probably broken")
if problems:
    sys.exit(
        f"error: {problems} action reference(s) are not pinned to a commit SHA. "
        f"For the generated release workflow, add the commit to "
        f"[dist.github-action-commits] in dist-workspace.toml and re-run `dist generate`."
    )
print(f"all {checked} third-party action references are pinned to commit SHAs")
PYEOF
