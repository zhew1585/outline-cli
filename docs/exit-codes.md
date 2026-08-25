# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body) |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403) |
| 5 | Resource not found | Unknown document, collection, or other resource (404) |
| 6 | Server error | Outline instance failed to process the request (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer |
| 8 | Rate limited | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |
| 9 | Partial failure | A batch command finished, but some items in the batch failed: `otl docs export` wrote some documents and could not write others |

Notes:

- Configuration errors (code 2) are always reported before any network request is made. A request that cannot even be assembled locally (invalid header value) is a configuration error, never a network error. The same holds for local schema validation: an argument the vendored spec rejects never reaches the network.
- A closed stdout pipe is normal completion, not a failure: when the reader stops early (`otl ... | head -1`), `otl` stops writing and exits **0** with no diagnostics, the way well-behaved Unix filters do. It never dies of a panic (which would produce the undocumented code 101).
- A response body that times out or is truncated mid-transfer is a network error (code 7), not an invalid-response error (code 1): only a genuine JSON syntax error means retrying cannot help.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- API errors (codes 3-6) print the sanitized server-provided message on stderr, plus the machine-readable error code (e.g. `[validation_error]`) when the server sent one. For a `--body` request the free-form message is withheld (it may quote the request body, which can contain secrets) and only a shape-validated error code is printed; `--show-server-message` opts back in. The exit code is unaffected either way.
- Network errors (code 7) and server errors (code 6) include a retry suggestion in the stderr message; only code 7 means the request may never have reached the server.
- Rate limiting has two outcomes: a 429 the retry budget absorbed succeeds normally, and a 429 that outlasted the budget exits **8**. Code 3 remains for any 429 surfaced without exhausting retries.
- Partial failure (code 9) is only for batch commands that deliberately keep going after one item fails. It means "the work that succeeded is real, and some of it is missing": `otl docs export` uses it when at least one document could not be fetched or written, and lists every failure on stderr. A batch that fails before doing any work at all reports the underlying error's own code instead, never 9. Enumerating the batch is not part of the batch: if the collection listing itself fails, that error's code (3-7) is returned and nothing is exported.
- A pager or browser launched by `otl docs view` is not part of the command's result: if `$PAGER` cannot be spawned the content is written straight to stdout with a stderr warning and the exit code stays 0, and a pager the user quits early is normal completion. A `--web` invocation that cannot launch a browser is a real failure (code 1) and prints the URL so it can be opened by hand.
