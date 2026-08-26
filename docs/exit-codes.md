# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe, a fetched OpenAPI document that cannot be compiled, a spec cache that cannot be written or deleted, a fetched document that is too large or not UTF-8 |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), unusable `otl spec sync` source (a `--spec` file that is missing, unreadable, oversized or not a usable OpenAPI document; a `--url` that is not a plain `http`/`https` URL) |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403) |
| 5 | Resource not found | Unknown document, collection, or other resource (404), spec document missing at its source URL (404) |
| 6 | Server error | Outline instance failed to process the request (5xx), spec host failed to serve the document (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer, unreachable spec source |
| 8 | Rate limited | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |

Notes:

- Configuration errors (code 2) are always reported before any network request is made. A request that cannot even be assembled locally (invalid header value) is a configuration error, never a network error. The same holds for local schema validation: an argument the vendored spec rejects never reaches the network.
- A closed stdout pipe is normal completion, not a failure: when the reader stops early (`otl ... | head -1`), `otl` stops writing and exits **0** with no diagnostics, the way well-behaved Unix filters do. It never dies of a panic (which would produce the undocumented code 101).
- A response body that times out or is truncated mid-transfer is a network error (code 7), not an invalid-response error (code 1): only a genuine JSON syntax error means retrying cannot help.
- `clap` usage errors (bad flags, missing subcommand) also exit with code 2.
- API errors (codes 3-6) print the sanitized server-provided message on stderr, plus the machine-readable error code (e.g. `[validation_error]`) when the server sent one. For a `--body` request the free-form message is withheld (it may quote the request body, which can contain secrets) and only a shape-validated error code is printed; `--show-server-message` opts back in. The exit code is unaffected either way.
- Network errors (code 7) and server errors (code 6) include a retry suggestion in the stderr message; only code 7 means the request may never have reached the server.
- A spec cache that cannot be used is **not** an error: `otl` discards it, falls back to the spec compiled into the binary, prints one warning on stderr naming the remedy, and the command's exit code is whatever it would have been anyway. This covers a damaged, truncated, foreign or version-mismatched cache file, one that is not a regular file (a pipe, device or symlink), and one that declares more operations than the format allows - a bad cache must never make the CLI unusable, hang it, or exhaust its memory. `otl spec reset` (exit 0 whether or not a cache existed) is the explicit way to drop it.
- A document that expands past what the parser will hold is refused while it is being parsed, not after: only the parts the compiler reads are materialized at all, and those are charged against a budget as they are built. The failure is classified like any other unusable document (code 2 for a `--spec` file the user named, 1 for a fetched one).
- A document that does not fit the cache format makes `otl spec sync` fail (code 1, or 2 for a `--spec` file the user named) rather than writing a cache the next command would discard. The three limits are reported separately, because they have different causes and different fixes: how many operations the document declares, how large one operation's parameters and enumerated values make it, and how much memory the whole table would occupy. Each error states the actual value, the limit, and what to do about it. All three are far above any real API: the vendored Outline spec uses about 16 KiB of a 1 MiB budget, 113 of 8192 operations, and 391 bytes of a 32 KiB per-operation allowance.
- `otl spec sync` classifies an unusable document by who chose it: a `--spec` file the user named is a usage error (code 2), like an invalid `--body` file, while a document fetched from a URL is a failure of that source (code 1). A `--spec` path that is not a regular file (a directory, FIFO, socket or device) is likewise code 2, refused before it is opened.
- The codes are the same for the Outline instance and for a spec source, but **the messages are not, and must not be**: a spec host is fetched anonymously, so its 401/403 (code 4) says the source needs credentials `spec sync` will not send, its transport failure (code 7) names that host, and neither ever mentions `OUTLINE_API_KEY` or `OUTLINE_URL` - which are not involved in fetching a spec. The two error domains are separate types in code (`EngineError` and `engine::fetch::FetchError`) precisely so this cannot drift.
- Rate limiting has two outcomes: a 429 the retry budget absorbed succeeds normally, and a 429 that outlasted the budget exits **8**. Code 3 remains for any 429 surfaced without exhausting retries. This applies to a rate-limiting spec source as well: `spec sync` retries with `Retry-After` and exits 8 when the budget runs out, exactly as an API call does.
