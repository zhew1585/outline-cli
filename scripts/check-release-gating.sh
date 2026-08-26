#!/usr/bin/env bash
# Assert that the guards in the generated release workflow can actually stop
# a release, rather than merely turn themselves red.
#
# This script exists because that distinction was got wrong once. cargo-dist's
# `host` job is guarded by
#
#     always() && ... && (needs.X.result == 'skipped' || needs.X.result == 'success')
#
# so a *skipped* dependency sails straight through and only a *failure*
# blocks. A gate registered as a `plan-jobs` job therefore does not gate
# anything: when it fails, the build jobs are skipped, not failed, and `host`
# proceeds. Two levers work, and this script asserts both are still wired:
#
#   1. `build-local-artifacts` failing - which is why the binary-size budget
#      is injected as a step in that job (github-build-setup).
#   2. a `local-artifacts-jobs` custom job failing - which is why the
#      preflight is registered there and not in `plan-jobs`. `host` lists
#      those jobs in `needs` and requires skipped-or-success from each.
#
# Everything downstream follows: host skipped -> publish-homebrew skipped
# (it has no `always()`) -> announce skipped (it requires host success) ->
# no GitHub Release, because `github-release = "announce"` puts the
# `gh release create` step in `announce`.
#
# The mirror image matters just as much: jobs that exist to *add* something
# (attestations) must not be able to block anything, or a transient failure
# turns a good build into a broken release. That is not obvious from the
# config - `host-jobs` reads like a safe slot but sits in
# publish-homebrew-formula's `needs`, so a failed attestation would skip the
# formula push while `announce` still published the Release. So this script
# also asserts that nothing on the publishing path depends on an attestation
# job, whichever slot it is registered in.
#
# It also asserts the build job's reduced token, which is a side effect of
# `github-attestations-phase` rather than a switch of its own, and two
# config settings that scripts/check-binary-size.sh assumes.
#
# All of this is "assert the property rather than trust the config": a
# cargo-dist upgrade can change the template silently and `dist generate
# --check` would still pass, because the file would match the new template.
#
# Usage:
#   ./scripts/check-release-gating.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

python3 - "${REPO_ROOT}" <<'PYEOF'
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
workflow_path = root / ".github/workflows/release.yml"
config_path = root / "dist-workspace.toml"

workflow = workflow_path.read_text(encoding="utf-8")
config = config_path.read_text(encoding="utf-8")

# The custom job name dist derives from the reusable workflow filename.
GUARD_JOB = "custom-release-guards"

JOB_HEADER = re.compile(r"^  ([A-Za-z0-9_-]+):$", re.MULTILINE)


def jobs(text: str) -> dict:
    """Slice the workflow into top-level job bodies without a YAML parser."""
    matches = list(JOB_HEADER.finditer(text))
    out = {}
    for index, match in enumerate(matches):
        end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
        out[match.group(1)] = text[match.start():end]
    return out


sections = jobs(workflow)
failures = []


def require(condition: bool, message: str) -> None:
    if condition:
        print(f"ok: {message}")
    else:
        failures.append(message)


for name in ("plan", GUARD_JOB, "build-local-artifacts", "host", "announce"):
    if name not in sections:
        failures.append(f"job `{name}` is missing from {workflow_path.name}")

if failures:
    for failure in failures:
        print(f"::error file={workflow_path}::{failure}", file=sys.stderr)
    sys.exit(f"error: {len(failures)} gating assertion(s) failed")

host = sections["host"]
build_local = sections["build-local-artifacts"]
announce = sections["announce"]
guard = sections[GUARD_JOB]

# 1. The preflight must be a dependency of `host`, and `host` must demand
#    skipped-or-success from it. This is what makes a preflight *failure*
#    block the release instead of being ignored.
require(
    re.search(rf"^\s+- {GUARD_JOB}$", host, re.MULTILINE) is not None,
    f"`host` lists {GUARD_JOB} in needs",
)
require(
    f"needs.{GUARD_JOB}.result == 'success'" in host,
    f"`host` requires {GUARD_JOB} to have succeeded (or been skipped)",
)

# 2. A failing build job must block `host` too - that is what carries the
#    binary-size budget.
require(
    "needs.build-local-artifacts.result == 'success'" in host,
    "`host` requires build-local-artifacts to have succeeded (or been skipped)",
)
require(
    "check-binary-size.sh" in build_local,
    "the binary-size budget runs inside build-local-artifacts",
)
require(
    "check-release-gating.sh" in build_local,
    "this gating check itself runs inside build-local-artifacts",
)

# 3. Least privilege for the only job that compiles the crate graph, i.e.
#    that executes dependency build scripts. This block is emitted solely
#    because `github-attestations-phase` is left at build-local-artifacts;
#    changing that key silently restores `contents: write`.
require(
    '"contents": "read"' in build_local,
    "build-local-artifacts drops the writable token (contents: read)",
)
require(
    '"attestations": "write"' in build_local,
    "build-local-artifacts can write attestations",
)

# 4. The release itself is created in `announce`, behind host success, so a
#    failed publish cannot leave a public half-release behind.
require(
    "needs.host.result == 'success'" in announce,
    "`announce` runs only when host succeeded",
)
require(
    "Create GitHub Release" in announce,
    "the GitHub Release is created in `announce`",
)
require(
    "Create GitHub Release" not in host,
    "the GitHub Release is NOT created in `host`",
)

# 5. The preflight must not be reachable only through a skip-tolerant path.
require(
    "uses: ./.github/workflows/release-guards.yml" in guard,
    "the preflight job calls the release-guards workflow",
)

# 6. Additive jobs must not gate anything. An attestation job that a
#    publishing job `needs` can skip that job, and a skipped publish reads
#    as success to `announce` - which is how a Release gets published with
#    no Homebrew formula pushed. Assert the dependency direction instead of
#    reasoning about which cargo-dist slot is safe.
ATTEST_PATTERN = re.compile(r"attest", re.IGNORECASE)
attest_jobs = [name for name in sections if ATTEST_PATTERN.search(name)]
require(bool(attest_jobs), "an attestation job is registered")

def needs_of(body: str) -> list:
    """The job names listed under this job's `needs:` block."""
    collected = []
    inside = False
    for line in body.splitlines():
        if line.strip() == "needs:":
            inside = True
            continue
        if inside:
            stripped = line.strip()
            if stripped.startswith("- "):
                collected.append(stripped[2:].strip())
                continue
            if stripped == "":
                continue
            break
    return collected


publishing_path = [
    name
    for name in sections
    if name in ("host", "announce") or name.startswith("publish-")
]
require(
    any(name.startswith("publish-") for name in publishing_path),
    "at least one publish job was found to check (guards against a silent no-op)",
)
for name in publishing_path:
    blockers = sorted(set(needs_of(sections[name])) & set(attest_jobs))
    require(
        not blockers,
        f"`{name}` does not depend on attestation job(s) "
        f"{blockers or '[]'} (a failed attestation must not skip publishing)",
    )

# And positively: the attestation job must run after the release, which is
# what `post-announce-jobs` buys.
for name in attest_jobs:
    require(
        "announce" in needs_of(sections[name]),
        f"`{name}` runs after `announce` (post-announce slot)",
    )

# 6. Two config settings scripts/check-binary-size.sh assumes when it
#    reproduces dist's RUSTFLAGS and build command. Enabling either makes
#    the measured binary differ from the shipped one.
require(
    re.search(r"^\s*msvc-crt-static\s*=", config, re.MULTILINE) is None,
    "msvc-crt-static is left at its default (check-binary-size.sh assumes +crt-static)",
)
require(
    re.search(r"^\s*cargo-auditable\s*=\s*true", config, re.MULTILINE) is None,
    "cargo-auditable is off (it would make the shipped binary larger than measured)",
)

if failures:
    for failure in failures:
        print(f"::error file={workflow_path}::{failure}", file=sys.stderr)
    sys.exit(
        f"error: {len(failures)} gating assertion(s) failed. The release pipeline may "
        f"no longer stop a bad release. Re-read scripts/check-release-gating.sh before "
        f"'fixing' this by relaxing the assertion."
    )
print("release gating chain verified")
PYEOF
