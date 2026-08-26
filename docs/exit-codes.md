# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), no credential configured for the active profile, unusable credential file (permissions too open, not a regular file, owned by another user, malformed, unknown format version, unreadable, unwritable, contended lock), credential directory writable by other users, stored credentials that belong to a different instance than the one being addressed, a non-loopback instance or OAuth endpoint reached over plaintext `http://`, unusable profile name, no loopback callback port available, an instance that does not offer dynamic client registration, a superseded client registration that could not be removed before replacing it, a write that would mix two instances' credentials in one profile, a stored OAuth endpoint that is not TLS-protected or no longer belongs to the instance in use, a login abandoned because a concurrent one finished first |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion, a 4xx from an OAuth endpoint that is not 401/403/429, a `logout --purge` whose server-side deletion failed (the local credentials are gone, the application is not) |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403), a stored OAuth session that can no longer be refreshed, a refresh whose rotated tokens could not be saved, authorization denied at the consent screen, a callback whose `state` did not match, a browser redirect that never arrived, OAuth metadata pointing an endpoint at another host, OAuth metadata whose `issuer` does not identify the instance it came from |
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
- Credentials are bound to the instance that issued them, and the binding is enforced on both sides. Pointing `OUTLINE_URL` at a different instance without switching profile exits **2** and sends nothing; so does a command that would ADD credentials for a second instance to the same profile, because a profile holding two instances' credentials would send one of them to the other.
- Plaintext `http://` exits **2** for every command, not just `auth login`, unless the host is a loopback IP literal. `localhost` is a name, not a literal, and does not qualify.
- `otl auth logout --purge` exits **3** when the local credentials were removed but the server refused to delete the application. The credential that manages that registration is deliberately kept on disk so the purge can be retried; it is the only thing that can ever delete it.
- A client registration stranded on the server (created, then neither saveable nor deletable) exits **1**: it is neither a local configuration problem nor retryable, and only an administrator can clear it.
