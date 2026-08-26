# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | Command completed normally |
| 1 | Generic failure | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe |
| 2 | Usage or configuration error | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, unsupported shell for `completions`, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), user-config-file problem, missing or ambiguous profile credential (both below), no credential configured for the active profile, unusable credential file (permissions too open, not a regular file, owned by another user, malformed, unknown format version, unreadable, unwritable, contended lock), credential directory writable by other users, stored credentials that belong to a different instance than the one being addressed, a non-loopback instance or OAuth endpoint reached over plaintext `http://`, no loopback callback port available, an instance that does not offer dynamic client registration, a superseded client registration that could not be removed before replacing it, a write that would mix two instances' credentials in one profile, a stored OAuth endpoint that is not TLS-protected or no longer belongs to the instance in use, a login abandoned because a concurrent one finished first |
| 3 | API request rejected | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion, a 4xx from an OAuth endpoint that is not 401/403/429 |
| 4 | Authentication or permission error | Invalid or expired API key (401), operation forbidden for this key (403), a stored OAuth session that can no longer be refreshed, a refresh whose rotated tokens could not be saved, authorization denied at the consent screen, a callback whose `state` did not match, a browser redirect that never arrived, OAuth metadata pointing an endpoint at another host, OAuth metadata whose `issuer` does not identify the instance it came from |
| 5 | Resource not found | Unknown document, collection, or other resource (404) |
| 6 | Server error | Outline instance failed to process the request (5xx) |
| 7 | Network error | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer |
| 8 | Rate limited | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |
| 9 | Partial failure | A command finished, but its result is incomplete: `otl docs export` could not write every document, a curated list command could not fetch every row, or `otl auth logout` could not complete a server-side step |

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
- Partial failure (code 9) means "what you got is real, and some of it is missing". Whatever succeeded is on stdout or on disk, every shortfall is described on stderr, and the exit code exists so that automation is never told a partial result is a complete one. Two situations produce it:
  - **An item failed.** `otl docs export` keeps going when one document cannot be fetched or written, then lists every failure. A command that fails before doing any work at all reports the underlying error's own code instead, never 9 — in particular, if the collection listing itself fails, that error's code (3-7) is returned and nothing is exported.
  - **The result set was cut short by the CLI's own pagination cap.** Auto-pagination stops after a fixed number of pages so a runaway list cannot loop forever. When it stops there, rows the server still had were never requested, so `otl docs search`, `otl collections list` and `otl docs export` all exit 9. `otl docs export` additionally reports `"complete": false` and `"enumeration_truncated": true` in its `--json` summary, because an output directory cannot show what was never fetched.
  - **A result that cannot be confirmed durable.** `otl docs export` flushes every directory it writes, including the ones it had to create to reach the output path (a directory's own name lives in its parent). If a flush *fails*, the files are readable now but are not known to survive a crash: the summary says so and `--json` reports `"durable": false`. A platform that offers no way to flush a directory at all is a different case and not a failure — the exit code stays 0 and `"durable"` is `null`, meaning "unknown", never `true`.

    Because `null` is falsy in most languages, `if (durable)` reads "unknown" as "failed". The test for *this backup is usable* is `complete == true && durable != false`.
  - **A listed item that could not be used.** `otl docs export` counts a listing row it cannot identify (no document id, an empty one, a non-string one) as a failed document rather than dropping it: the server said that document exists, so an export that skipped it is not complete. A row that merely repeats an id already seen is *not* a failure — that document is exported once and nothing is missing.
- A truncation the caller ASKED for is not a failure: `--limit N` does exactly what it says, so it warns on stderr and exits **0**. This holds for `otl docs export` too — a `--limit`ed export is a deliberate partial copy, and its `--json` summary says so with `"limit_reached": true` while keeping `"complete": true`, so `complete && !limit_reached` is the test for "this is the whole collection". Only the CLI giving up on its own becomes code 9. (`otl api` is unchanged: it warns and exits 0 in both cases, and its output is explicitly unstable.)
- A pager or browser launched by `otl docs view` is not part of the command's result: if `$PAGER` cannot be spawned the content is written straight to stdout with a stderr warning and the exit code stays 0, and a pager the user quits early is normal completion. A `--web` invocation that cannot launch a browser is a real failure (code 1) and prints the URL so it can be opened by hand.
- Authentication failures split along one line, so a script can branch on it: **4** means "authenticate again" (`otl auth login`, or a new API key), **2** means "fix something locally" (a permission bit, a missing environment variable, a client id an administrator has to create). A credential file whose permissions are too wide is code **2**, not 4: the credential may well be valid, but it is refused until the file is tightened.
- A refresh that succeeds on the server but cannot be written to disk exits **4**, not 1: Outline rotates the refresh token on every use, so the stored one is already dead and the only way forward is a new login. This is reported explicitly and never silently retried.
- Credentials are bound to the instance that issued them, and the binding is enforced on both sides. Pointing `OUTLINE_URL` at a different instance without switching profile exits **2** and sends nothing; so does a command that would ADD credentials for a second instance to the same profile, because a profile holding two instances' credentials would send one of them to the other.
- Plaintext `http://` exits **2** for every command, not just `auth login`, unless the host is a loopback IP literal. `localhost` is a name, not a literal, and does not qualify.
- `otl auth logout` exits **9** whenever a server-side step did not happen — a token that could not be revoked, or a `--purge` deletion the server refused. Two rules go with that code:
  - **If a retry could still succeed, the local credentials are kept.** Discarding the only copy of a token that is still live on the server would make it permanently unrevocable, so the default is to keep it and say so. `--force` discards anyway, for a user who accepts that.
  - **If no retry could succeed** — the instance advertises no revocation endpoint, a stored endpoint is plaintext or belongs to another instance, the server rejected the credential itself (400/401/403), or the registration has no management token — the credentials are removed, because keeping them buys nothing. The exit code is still 9, since what was asked for did not happen. These cases say so rather than advising a retry that cannot work.
  - **If a credential written by another process survived** — a concurrent refresh landing a rotated session inside the revocation window — it is reported and the exit code is 9. That session was never revoked, so `revoked` is `false` and the command does not claim to have signed out. Deleting another process's session is never the answer; run `otl auth logout` again to revoke and remove it.
- `otl auth logout` never requires `OUTLINE_URL`, and never applies the transport rule to it. Everything it contacts comes out of the credential file, anchored to the origin each credential recorded for itself. That is deliberate: cleanup has to work precisely when the configuration is missing, wrong, or predates a rule, and the alternative is a user deleting the file by hand and orphaning a DCR registration for good.
- A client registration stranded on the server (created, then neither saveable nor deletable) exits **1**: it is neither a local configuration problem nor retryable, and only an administrator can clear it.
