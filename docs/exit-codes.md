# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, unsupported shell for `completions`, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), user-config-file problem, missing or ambiguous profile credential (both below) |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403) |
| 5 | Resource not found | Unknown document, collection, or other resource (404) |
| 6 | Server error | Outline instance failed to process the request (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer |
| 8 | Rate limited | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |

Notes:

- Every profile-scope failure is a configuration error (code 2), reported before any request:
  - **missing profile key** - the selected profile's `OUTLINE_API_KEY_<PROFILE>` is unset or blank. The
    global `OUTLINE_API_KEY` is never a fallback for a profile;
  - **unusable variable name** - the profile name contains no ASCII letter or digit, or is longer than
    64 characters, so it names no usable variable;
  - **ambiguous variable** - two profiles map to the same variable;
  - **conflicting URL** - the base URL was resolved from `OUTLINE_URL` and its origin differs from the
    one the selected profile declares;
  - **unbound credential** - the base URL was resolved from `OUTLINE_URL` and the selected profile
    declares no `url`, so there is nothing to bind its credential to;
  - **unusable profile URL** - the base URL was resolved from `OUTLINE_URL` and the selected profile's
    own `url` is not a usable base URL, so no binding can be established. (A resolved URL that is itself
    unusable is not this error: nothing can be sent to it, so it goes to the request channel, which
    reports `invalid base URL`.)
  - **missing URL** - no layer supplied a base URL at all.

  Resolution itself always follows flag > env > file, for the base URL as for every other key. The three
  URL-related failures are raised at the credential-release boundary, which asks the separate question of
  whether the resolved origin is one the resolved credential belongs to: a credential that has been sent
  to the wrong server cannot be recalled, so a mismatch fails instead of warning. Origins are compared
  normalized, so a trailing slash, host casing or a default port is never a conflict. `--url` is the
  documented way to redirect a profile deliberately, and is stated in the same command as `--profile`.
- Every user-config-file failure is a configuration error (code 2), never a new code: a file named
  explicitly with `--config` / `OUTLINE_CONFIG` that does not exist, one that cannot be read or is
  larger than the size cap, TOML that does not parse, an unknown key, an empty profile name, a
  `--profile` / `OUTLINE_PROFILE` / `default_profile` naming a profile the file does not define, an
  `auth` method the build cannot use yet, or an `api_key` / `token` key in the config file (credentials
  belong in `credentials.toml`). A config file missing at the DEFAULT location is not an error at all:
  the environment-only path must keep working on a fresh machine.
- Config-file parse errors report a line number, a description this CLI owns, and the full config
  schema - never any text produced by the TOML parser. The parser's messages interpolate the offending
  VALUE (an unknown `auth` value, a type mismatch, an unknown bare key), so a credential wrongly placed
  in the config file would otherwise be echoed back into a message, a log, or a Debug rendering. Names
  and paths that are shown (a `--profile` argument, the list of defined profiles, a `--config` path) have
  their control characters replaced and their length capped, because a TOML quoted key and a path
  argument can both carry ESC, BEL or newline bytes - enough to forge a terminal hyperlink or an extra
  `error:` line. `Debug` renderings go further and omit profile names entirely: `Display` is the only
  place a name is needed, and `Debug` is the surface that ends up in logs and panic messages.
- Configuration errors (code 2) are always reported before any network request is made. A request that cannot even be assembled locally (invalid header value) is a configuration error, never a network error. The same holds for local schema validation: an argument the vendored spec rejects never reaches the network.
- A closed stdout pipe is normal completion, not a failure: when the reader stops early (`otl ... | head -1`), `otl` stops writing and exits **0** with no diagnostics, the way well-behaved Unix filters do. It never dies of a panic (which would produce the undocumented code 101).
- A response body that times out or is truncated mid-transfer is a network error (code 7), not an invalid-response error (code 1): only a genuine JSON syntax error means retrying cannot help.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- API errors (codes 3-6) print the sanitized server-provided message on stderr, plus the machine-readable error code (e.g. `[validation_error]`) when the server sent one. For a `--body` request the free-form message is withheld (it may quote the request body, which can contain secrets) and only a shape-validated error code is printed; `--show-server-message` opts back in. The exit code is unaffected either way.
- Network errors (code 7) and server errors (code 6) include a retry suggestion in the stderr message; only code 7 means the request may never have reached the server.
- Rate limiting has two outcomes: a 429 the retry budget absorbed succeeds normally, and a 429 that outlasted the budget exits **8**. Code 3 remains for any 429 surfaced without exhausting retries.
