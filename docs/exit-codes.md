# Exit codes

The exit-code table is part of the public API of `otl`.
Published codes never change meaning.
New error classes must be registered here before release.
The single source of truth in code is the `ExitCode` enum in `crates/otl/src/exit.rs`.

This table is also the source of two generated copies, both kept in step by
`crates/otl/tests/exit_code_tables.rs`: the condensed table in `README.md`
(Code + Meaning) and the one in the shipped agent skill
(`crates/otl/skill/SKILL.md`, Code + `Meaning: Agent summary`).
The **Agent summary** column exists for the second of those: an agent reading
only "Generic failure" cannot act on it, and the Examples column is far too
long to put in front of a model on every failure. Keep each summary to the few
words that change what a caller does next.

| Code | Meaning | Agent summary | Examples |
|------|---------|---------------|----------|
| 0 | Success | also a closed stdout pipe (`otl ... \| head`), which is normal completion | Command completed normally |
| 1 | Generic failure | a response that is not JSON, an internal error | Invalid JSON in a response, unexpected internal error, HTTP status outside 4xx/5xx, failure writing to stdout other than a closed pipe, a fetched OpenAPI document that cannot be compiled, a spec cache that cannot be written or deleted, a fetched document that is too large or not UTF-8, a response whose status was valid but whose required shape was not (an expected HTTP 302 without a usable absolute `http(s)` `Location`, or a 200 where that redirect was required) |
| 2 | Usage or configuration error | bad flag, unknown operation, missing `OUTLINE_URL`, local validation failure, config-file problem, credential file permissions too open, plaintext `http://` | Unknown subcommand or flag, malformed `key=value` argument, unknown API operation, unsupported shell for `completions`, missing `OUTLINE_URL` / `OUTLINE_API_KEY`, invalid base URL, API key that cannot be sent as an HTTP header (e.g. it contains a newline), local parameter-validation failure (unknown/missing/complex parameter, value violating its schema facets, inexact number, oversized or invalid `--body` file, operation requiring a non-JSON body or a `oneOf`/`anyOf` request body), user-config-file problem, missing or ambiguous profile credential (both below), no credential configured for the active profile, unusable credential file (permissions too open, not a regular file, owned by another user, malformed, unknown format version, unreadable, unwritable, contended lock), credential directory writable by other users, stored credentials that belong to a different instance than the one being addressed, a non-loopback instance or OAuth endpoint reached over plaintext `http://`, no loopback callback port available, an instance that does not offer dynamic client registration, a superseded client registration that could not be removed before replacing it, a write that would mix two instances' credentials in one profile, a stored OAuth endpoint that is not TLS-protected or no longer belongs to the instance in use, a login abandoned because a concurrent one finished first, unusable `otl spec sync` source (a `--spec` file that is missing, unreadable, oversized, not a regular file, or not a usable OpenAPI document; a `--url` that is not a plain `http`/`https` URL), `otl skill install` with no skills directory to write to, or refusing every target it had (another skill's `SKILL.md` without `--force`, or a path that is not a regular file), `otl docs update` with no document to write to (no ID argument and no `outline_id` in the file's frontmatter), an ID argument that contradicts the file's frontmatter, or a `--file` whose recorded `revision` is behind the document's current one (re-export and reapply, or pass `--force`) |
| 3 | API request rejected | a 4xx that is not auth, not-found or exhausted rate limits | 4xx other than auth, not-found, and exhausted rate limits: validation error (400), a 429 that was not retried to exhaustion, a 4xx from an OAuth endpoint that is not 401/403/429 |
| 4 | Authentication or permission error | authenticate again | Invalid or expired API key (401), operation forbidden for this key (403), a stored OAuth session that can no longer be refreshed, a refresh whose rotated tokens could not be saved, authorization denied at the consent screen, a callback whose `state` did not match, a browser redirect that never arrived, OAuth metadata pointing an endpoint at another host, OAuth metadata whose `issuer` does not identify the instance it came from |
| 5 | Resource not found | the document, collection or other resource does not exist | Unknown document, collection, or other resource (404), spec document missing at its source URL (404) |
| 6 | Server error | the instance failed to process the request | Outline instance failed to process the request (5xx), spec host failed to serve the document (5xx) |
| 7 | Network error | the request may never have arrived | DNS failure, connection refused, TLS failure, request timeout, response body that times out or is cut mid-transfer, unreachable spec source |
| 8 | Rate limited | the retry budget was exhausted, so retry later | The server kept answering HTTP 429 until the retry budget was exhausted; retry later |
| 9 | Partial failure | what you got is real, and some of it is missing | A command finished, but its result is incomplete: `otl docs export` could not write every document, a curated list command could not fetch every row, `otl auth logout` could not complete a server-side step, or `otl skill install` installed into some of its targets and not others |

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
- A spec cache that cannot be used is **not** an error: `otl` discards it, falls back to the spec compiled into the binary, prints one warning on stderr naming the remedy, and the command's exit code is whatever it would have been anyway. This covers a damaged, truncated, foreign or version-mismatched cache file, one that is not a regular file (a pipe, device or symlink), and one that declares more operations than the format allows - a bad cache must never make the CLI unusable, hang it, or exhaust its memory. `otl spec reset` (exit 0 whether or not a cache existed) is the explicit way to drop it.
- A document that expands past what the parser will hold is refused while it is being parsed, not after: only the parts the compiler reads are materialized at all, and those are charged against a budget as they are built. The failure is classified like any other unusable document (code 2 for a `--spec` file the user named, 1 for a fetched one).
- A document that does not fit the cache format makes `otl spec sync` fail (code 1, or 2 for a `--spec` file the user named) rather than writing a cache the next command would discard. The three limits are reported separately, because they have different causes and different fixes: how many operations the document declares, how large one operation's parameters and enumerated values make it, and how much memory the whole table would occupy. Each error states the actual value, the limit, and what to do about it. All three are far above any real API: the vendored Outline spec uses about 16 KiB of a 1 MiB budget, 113 of 8192 operations, and 391 bytes of a 32 KiB per-operation allowance.
- `otl spec sync` classifies an unusable document by who chose it: a `--spec` file the user named is a usage error (code 2), like an invalid `--body` file, while a document fetched from a URL is a failure of that source (code 1). A `--spec` path that is not a regular file (a directory, FIFO, socket or device) is likewise code 2, refused before it is opened.
- The codes are the same for the Outline instance and for a spec source, but **the messages are not, and must not be**: a spec host is fetched anonymously, so its 401/403 (code 4) says the source needs credentials `spec sync` will not send, its transport failure (code 7) names that host, and neither ever mentions `OUTLINE_API_KEY` or `OUTLINE_URL` - which are not involved in fetching a spec. The two error domains are separate types in code (`EngineError` and `engine::fetch::FetchError`) precisely so this cannot drift.
- Rate limiting has two outcomes: a 429 the retry budget absorbed succeeds normally, and a 429 that outlasted the budget exits **8**. Code 3 remains for any 429 surfaced without exhausting retries. This applies to a rate-limiting spec source as well: `spec sync` retries with `Retry-After` and exits 8 when the budget runs out, exactly as an API call does.
- Partial failure (code 9) means "what you got is real, and some of it is missing". Whatever succeeded is on stdout or on disk, every shortfall is described on stderr, and the exit code exists so that automation is never told a partial result is a complete one. Two situations produce it:
  - **An item failed.** `otl docs export` keeps going when one document cannot be fetched or written, then lists every failure. A command that fails before doing any work at all reports the underlying error's own code instead, never 9 — in particular, if the collection listing itself fails, that error's code (3-7) is returned and nothing is exported.
  - **The result set was cut short by the CLI's own pagination cap.** Auto-pagination stops after a fixed number of pages so a runaway list cannot loop forever. When it stops there, rows the server still had were never requested, so `otl docs search`, `otl collections list` and `otl docs export` all exit 9. `otl docs export` additionally reports `"complete": false` and `"enumeration_truncated": true` in its `--json` summary, because an output directory cannot show what was never fetched.
  - **A result that cannot be confirmed durable.** `otl docs export` flushes every directory it writes, including the ones it had to create to reach the output path (a directory's own name lives in its parent). If a flush *fails*, the files are readable now but are not known to survive a crash: the summary says so and `--json` reports `"durable": false`. A platform that offers no way to flush a directory at all is a different case and not a failure — the exit code stays 0 and `"durable"` is `null`, meaning "unknown", never `true`.

    Because `null` is falsy in most languages, `if (durable)` reads "unknown" as "failed". The test for *this backup is usable* is `complete == true && durable != false`.
  - **A listed item that could not be used.** `otl docs export` counts a listing row it cannot identify (no document id, an empty one, a non-string one) as a failed document rather than dropping it: the server said that document exists, so an export that skipped it is not complete. A row that merely repeats an id already seen is *not* a failure — that document is exported once and nothing is missing.
- `otl api`'s two local discovery paths — `otl api list` and `otl api describe <operation>`, the second
  of which is also what `otl api <operation> --help` prints — introduce **no exit code of their own**.
  They read the operation table already compiled into the binary (or the one `otl spec sync` installed),
  send nothing, and exit **0**. Every way they can fail is an existing code 2: an unknown operation name
  (the same message and the same code the call path gives, so the two can never disagree), `describe`
  with no operation or more than one, and a request-shaping flag (`--body`, `--no-validate`,
  `--show-server-message`, `--limit`) on a path that will not send a request. An unknown operation name
  passed with `--help` is that same code 2 and **not** a fallback to the command's generic help:
  answering a question about `documents.inf` with authoritative-looking text about `otl api` in general
  is worse than an error, because an error makes a caller try something else.
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
- `otl skill install` writes a local file and introduces no exit code of its own either. Nothing to write
  to at all (no agent skills directory, and no `--dir`) is a usage error (2), like any other invocation
  that cannot be carried out. A target it refuses - another skill's `SKILL.md` without `--force`, or a
  path that is not a regular file, which `--force` deliberately does NOT override - is also 2 when it
  refused every target, and **9** when it wrote some and refused others, because what was installed is
  real and something asked for did not happen. A write that fails outright is a plain failure (1). The
  skill check in `otl doctor` never blocks: a stale or absent skill stops nothing, so it is a warning or
  an `ok`, never a problem.

- `otl doctor` introduces **no exit code of its own**, and that is the point: it answers "is this
  environment usable?", so it exits with the code the first blocking finding *would have produced in any
  other command*. 0 means nothing is blocking. 2 is something to fix locally (an unusable config file, a
  credential file whose permissions are too wide or whose contents cannot be read, no credential
  configured, an instance URL that is missing, plaintext or unusable, a `--spec-url` this CLI will not
  fetch). 4 means the instance rejected the credential. 1, 3, 5, 6, 7 and 8 are whatever the instance
  answered, or failed to answer, to the one probe `doctor` sends (`auth.info` through the ordinary request
  channel) - 1 covers a 200 carrying something that is not JSON, exactly as it would for `otl api`. Code
  9 is not reachable: `doctor` produces no partial result. Five rules go with that:
  - **First, not worst.** The checks run in dependency order - config file, instance URL, credential
    file, chosen credential, reachability, local spec, online spec, agent skill - and the FIRST blocking one decides
    the code. An earlier problem is both the cause of what follows and the thing to fix first: reporting
    the numerically highest code instead would point a user at a network failure that is really a missing
    `OUTLINE_URL`.
  - **A warning is never blocking.** A spec cache that had to be discarded, a local table behind the
    online one, an unreachable spec host, a plaintext key in the environment, a credential DIRECTORY other
    users can write to around a sound owner-only file, an installed agent skill whose version is not this
    binary's: all are reported, none changes the exit code,
    because none of them stops `otl` from working. In particular a spec host is a third party the CLI
    consults only when asked, so its 404 or its firewall must never make `otl doctor` call a working
    environment broken. A `--spec-url` value the fetch channel refuses locally is *not* in this group: it
    is the invocation being wrong, so it is code 2, like the same mistake in `otl spec sync`.
  - **The credential FILE and the directory around it are graded apart.** The file itself unusable - mode
    widened, not a regular file, owned by someone else, malformed, from a newer version - is code 2, and
    nothing is sent, because `read_checked` refuses it on the open descriptor. A world-writable DIRECTORY
    holding a sound `0600` file is a warning and the credential is used: another user cannot plant a file
    the caller owns (the ownership check is on the open descriptor, and symlinks are refused), cannot read
    the `0600` file, and Story 2.6 deliberately does not re-permission an existing directory. The residual
    risk is deletion or a replacement that then gets refused - nuisance, not disclosure - and no other
    command fails in that state either, so `doctor` must not. The *text* obeys the same rule: a report
    that is about to use the file does not print the store-wide "usable: no", and does not repeat the
    write path's "refusing to use it" - that sentence is true where a write or the refresh lock is
    refused, and false of the read this report describes. A symlink at the credential path, dangling or
    not, is a FILE problem (code 2): the read opens `O_NOFOLLOW`, so the report is built from
    `symlink_metadata` and gives the same answer rather than calling the path empty.
  - **The report is always printed**, before the code is decided and whatever the code turns out to be.
    A `--json` consumer gets the same object on every run; a blocking finding is additionally summarized
    on stderr, naming the check it came from. The connectivity summary never overstates what happened:
    only a transport failure (7) says the instance could not be reached, because only that code means the
    request may never have arrived. A 401, a 500 or a non-JSON body all report that the instance answered,
    and `"reachable"` is `true` for them. A failure that never left this machine - a credential that
    cannot be expressed as an HTTP header, a stored session that could not be renewed before the call -
    reports `"reachable": false` and says nothing was sent. That distinction cannot be read off the exit
    code, which is why it is decided separately: code 2 covers both a header this machine could not build
    (nothing sent) and a parameter the spec refused (nothing sent), while code 1 covers both a client that
    could not be built (nothing sent) and a reply that was not JSON (sent, and answered).
  - **`doctor` classifies nothing itself.** Every blocking code comes from the same mapper the failing
    command uses (`auth::exit_code_of`, the borrowing half of `map_auth_error`), so a diagnosis cannot
    disagree with the command it is diagnosing.
