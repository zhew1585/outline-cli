# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline) |
| 3 | API request rejected | 4xx other than auth/not-found: validation error (400), rate limit (429, until a dedicated class is registered) |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403) |
| 5 | Resource not found | Unknown document, collection, or other resource (404) |
| 6 | Server error | Outline instance failed to process the request (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer |

Notes:

- Configuration errors (code 2) are always reported before any network request is made. A request that cannot even be assembled locally (invalid header value) is a configuration error, never a network error.
- A closed stdout pipe is normal completion, not a failure: when the reader stops early (`otl ... | head -1`), `otl` stops writing and exits **0** with no diagnostics, the way well-behaved Unix filters do. It never dies of a panic (which would produce the undocumented code 101).
- A response body that times out or is truncated mid-transfer is a network error (code 7), not an invalid-response error (code 1): only a genuine JSON syntax error means retrying cannot help.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- API errors (codes 3-6) print the sanitized server-provided message on stderr, plus the machine-readable error code (e.g. `[validation_error]`) when the server sent one.
- Network errors (code 7) and server errors (code 6) include a retry suggestion in the stderr message; only code 7 means the request may never have reached the server.
- A dedicated rate-limit class may be registered in a later story; until then 429 falls under code 3. Existing codes keep their meaning.
