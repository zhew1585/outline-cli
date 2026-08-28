# Changelog

Notable changes to `otl`.
This project is pre-1.0: breaking changes may land in a minor release until 1.0.

`otl api <operation>` is explicitly excluded from that promise — its shape is the server's, not this CLI's.
The curated commands, the exit-code table and the `--json` shapes are the contract.

## 0.1.0 - 2026-08-28

First release. macOS only (`aarch64-apple-darwin` and `x86_64-apple-darwin`); Linux and Windows are neither built nor tested.

### Two ways to reach the API

Outline's API is pure RPC — every endpoint is `POST /api/resource.method` — so the OpenAPI spec is the contract.
`otl` compiles that spec into a static table at build time and interprets it at runtime, which is what makes every endpoint callable without hand-written per-endpoint code.

- `otl api <operation>` calls any operation the spec declares, by name.
  Arguments are `key=value` pairs, coerced to the types the spec declares and validated locally, so a bad UUID exits 2 without a request going out.
  `--body @file.json` passes `oneOf`/`anyOf` bodies through verbatim.
- `otl api list` and `otl api describe <operation>` are purely local: they read the table compiled into the binary and send nothing.
  Both name the stable command for an operation when one exists, so the contracted path is reachable from the generic one.
- `otl spec sync` pulls a newer spec when upstream adds an endpoint; `otl spec reset` returns to the one built into the binary.

### Curated commands

Flags and output here are a stable contract, unlike `otl api`.

- `otl docs` — `search`, `list`, `view`, `create`, `update`, `move`, `delete`, `export`.
- `otl collections` — `list`, `create`, `update`, `delete`.
- `otl comments` — `list`, `create`, `update`, `delete`.
- `otl fetch` — `document`, `collection`, `user`, `attachment`, addressable by URL as well as id.
- `otl attachments create`, `otl users list`.

Each command's `--help` ends in an `API contract:` section naming the operation it drives and a `JSON shape:` section stating what it writes to stdout.

Details worth knowing before scripting against it:

- **`docs view` is markdown-first.**
  A pipe gets the document body, because the body *is* the data; `--json` gets the document object instead.
  `--outline` is the exception — its datum is structure, so a pipe gets JSON.
- **Section-level edits.**
  `--section 'Deploy'` reads the body, splices it, and derives a `findText` that occurs exactly once, so only the changed part is sent and no repeated heading can be patched by mistake.
  Address a section by title, by parent (`'Deploy > Rollback'`), or with the level pinned (`'## Deploy'`); an ambiguous address is refused with every match listed.
- **Exported files name their document.**
  Each file opens with a YAML block carrying `outline_id`, `outline_url_id`, `title`, `revision` and `updated_at`, so an export can be written back rather than being a copy you can only read.
  `docs update --file` takes the id and the revision pin from that block.
- **Writes are pinnable.**
  `--if-revision <n>` is checked by this CLI before anything is sent, on every mode, and also travels as `lastRevision` for instances that enforce it server-side.
  Section edits and `--mode patch` pin themselves to the revision they read.
- **Pagination never truncates silently.**
  `--limit N` is a cap you asked for: it warns and exits 0.
  The CLI's own page cap stopping a fetch early is not, and exits 9.

### Authentication

- `otl auth login` — OAuth 2.0 authorization code with PKCE in the browser.
  It discovers the instance's endpoints, registers `otl` as a public client via RFC 7591 dynamic registration when the instance allows it, and catches the redirect on a loopback port.
  `--client-id` covers the pre-registered case.
- `otl auth set-key` stores an API key in a `0600` credential file instead of your shell history; `OUTLINE_API_KEY` also works everywhere with no setup.
- Tokens are renewed inside the request channel, so no command fails because a token aged out.
- `otl auth logout` revokes and forgets a profile's credentials; `otl auth info` reports which credential is in use and where it lives.

Security properties this release commits to: TLS is required unless the host is a loopback IP literal, credential-bearing requests never follow redirects, a credential is only ever sent to the instance that issued it, and the credential file is created owner-only rather than created and then `chmod`ed.

### Configuration

Three layers, resolved flag > environment > user config file, key by key, with named profiles in `config.toml`.

A profile reads its API key from its own variable (`OUTLINE_API_KEY_WORK` for `--profile work`) and never falls back to the global one, because that would send one workspace's credential to another workspace's server.
The config file holds no secrets by construction: an `api_key` or `token` key there is a hard error pointing at `credentials.toml`.

### Output

- Dual-state: a table on a terminal, JSON whenever stdout is not one.
  Table columns are picked from the response schema rather than hard-coded.
- Ten exit codes, from 0 (success) to 9 (partial failure), documented in `docs/exit-codes.md` — the source of truth that the tables in `README.md` and the agent skill are tested against.
  A closed stdout pipe is not a failure: `otl ... | head -1` exits 0.
- In JSON mode a failure is also an object on stderr while stdout stays empty.
  Argument errors caught by the parser keep the prose form, so a caller must not assume the object is always there; the exit code is the fact that always is.
- Automatic pagination, 429 backoff and a global throttle.

### Agents and shells

- `otl doctor` reports credentials, instance reachability and spec drift in one pass; `--offline` contacts nothing.
- `otl skill install` installs a bundled agent skill describing the CLI's surface.
- `otl completions <shell>` generates completions for bash, zsh, fish, powershell and elvish, from the same compiled table.

### Distribution

- One shell installer, no package manager: it detects your Apple target, unpacks `otl` into `~/.local/bin`, and adds that directory to `PATH` unless it is already there.
  Nothing is installed outside your home directory and no sudo is needed.
  `OUTLINE_CLI_INSTALL_DIR` and `OUTLINE_CLI_NO_MODIFY_PATH=1` adjust both behaviours.
- Every artifact carries a GitHub build attestation, verifiable with `gh attestation verify`.
- **No phone home.**
  `otl` never checks for updates and sends no telemetry — it makes exactly the network requests your command implies.
  The auto-updater cargo-dist can ship is disabled on purpose, and the release pipeline fails if an updater artifact ever appears in the plan.
- Cold start stays in single-digit milliseconds, and the shipped binary stays under 5 MB.
  Both are enforced as gates in CI rather than left as aspirations.
