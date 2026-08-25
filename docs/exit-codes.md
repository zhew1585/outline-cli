# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), no credential configured for the active profile, unusable credential file (permissions too open, malformed, unknown format version, unreadable, unwritable, contended refresh lock), unusable profile name, no loopback callback port available, an instance that does not offer dynamic client registration |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion, a 4xx from an OAuth endpoint that is not 401/403/429 |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403), a stored OAuth session that can no longer be refreshed, a refresh whose rotated tokens could not be saved, authorization denied at the consent screen, a callback whose `state` did not match, a browser redirect that never arrived, OAuth metadata pointing an endpoint at another host |
| 5 | Resource not found | Unknown document, collection, or other resource (404) |
| 6 | Server error | Outline instance failed to process the request (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer |
| 8 | Rate limited | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |

Notes:

- Configuration errors (code 2) are always reported before any network request is made. A request that cannot even be assembled locally (invalid header value) is a configuration error, never a network error. The same holds for local schema validation: an argument the vendored spec rejects never reaches the network.
- A closed stdout pipe is normal completion, not a failure: when the reader stops early (`otl ... | head -1`), `otl` stops writing and exits **0** with no diagnostics, the way well-behaved Unix filters do. It never dies of a panic (which would produce the undocumented code 101).
- A response body that times out or is truncated mid-transfer is a network error (code 7), not an invalid-response error (code 1): only a genuine JSON syntax error means retrying cannot help.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- API errors (codes 3-6) print the sanitized server-provided message on stderr, plus the machine-readable error code (e.g. `[validation_error]`) when the server sent one. For a `--body` request the free-form message is withheld (it may quote the request body, which can contain secrets) and only a shape-validated error code is printed; `--show-server-message` opts back in. The exit code is unaffected either way.
- Network errors (code 7) and server errors (code 6) include a retry suggestion in the stderr message; only code 7 means the request may never have reached the server.
- Rate limiting has two outcomes: a 429 the retry budget absorbed succeeds normally, and a 429 that outlasted the budget exits **8**. Code 3 remains for any 429 surfaced without exhausting retries.
- Authentication failures split along one line, so a script can branch on it: **4** means "authenticate again" (`otl auth login`, or a new API key), **2** means "fix something locally" (a permission bit, a missing environment variable, a client id an administrator has to create). A credential file whose permissions are too wide is code **2**, not 4: the credential may well be valid, but it is refused until the file is tightened.
- A refresh that succeeds on the server but cannot be written to disk exits **4**, not 1: Outline rotates the refresh token on every use, so the stored one is already dead and the only way forward is a new login. This is reported explicitly and never silently retried.
