# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Network/transport error, server-side API error, unexpected internal error |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, local parameter-validation failure (unknown/missing/complex parameter, value violating its schema, oversized or invalid `--body` file, operation requiring a non-JSON body) |

Notes:

- Configuration errors (code 2) are always reported before any network request is made.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- This table will be extended by later stories (e.g. dedicated codes for auth and rate-limit classes); existing codes keep their meaning.
