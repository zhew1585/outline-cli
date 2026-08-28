# outline-cli (`otl`)

A fast, single-binary CLI for [Outline](https://www.getoutline.com/) knowledge bases.

Outline's API is pure RPC — every endpoint is `POST /api/resource.method` — so the OpenAPI spec is the contract.
`otl` compiles that spec into a static table at build time and interprets it at runtime.
Every endpoint is therefore callable without hand-written per-endpoint code, and cold start stays in single-digit milliseconds.

Two ways to reach the API, and the difference matters:

- **Curated commands** (`otl docs`, `otl collections`, `otl comments`, …) — flags and output are a stable contract.
- **`otl api <operation>`** — any operation the spec declares, by name. Explicitly unstable: its shape is the server's, not this CLI's.

> **Status: pre-1.0.** The commands below are implemented and covered by tests. Breaking changes may still land in a minor release before 1.0.

## Install

**macOS only, for now.** Linux and Windows are not built or tested at the moment.

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/zhew1585/outline-cli/releases/latest/download/outline-cli-installer.sh | sh
```

The script detects your Apple target, unpacks `otl` into `~/.local/bin`, and adds that directory to `PATH` via your shell profiles unless it is already there.
No sudo, no package manager, nothing installed outside your home directory.
Set `OUTLINE_CLI_INSTALL_DIR` to install elsewhere, or `OUTLINE_CLI_NO_MODIFY_PATH=1` to leave shell profiles alone.

Every release artifact carries a GitHub build attestation:

```sh
gh attestation verify outline-cli-aarch64-apple-darwin.tar.xz --repo zhew1585/outline-cli
```

`otl` never checks for updates and sends no telemetry — it makes exactly the network requests your command implies.
To upgrade, re-run the installer.

**From source** (Rust stable):

```sh
git clone https://github.com/zhew1585/outline-cli
cd outline-cli
cargo build --release          # binary at target/release/otl
```

## Quick start

```sh
export OUTLINE_URL=https://outline.example.com
export OUTLINE_API_KEY=...             # Settings → API in your Outline instance

otl docs search deploy                 # title / collection / updated / snippet
otl docs view <doc-id>                 # markdown, through $PAGER on a terminal
otl api list                           # every callable operation
otl api describe documents.info        # what it takes and what it returns
otl api documents.info id=<doc-id>     # call any of them
```

`otl api` arguments are `key=value` pairs, coerced to the types the spec declares and validated locally before any request goes out:

```sh
otl api documents.list limit=25 template=false   # native JSON number and boolean on the wire
otl api documents.info id=not-a-uuid             # exits 2, no request sent
otl api shares.create --body @share.json         # oneOf/anyOf bodies go through --body verbatim
```

`otl api list` and `otl api describe` are purely local — they read the operation table compiled into the binary and send nothing.
Both also name the stable command for an operation when one exists (`curated_command`), so you can find the contracted path from the generic one.

## Signing in

An API key in the environment works everywhere and needs no setup.
For interactive use there is a browser flow, and for keys there is somewhere better than your shell history:

```sh
otl auth login                         # OAuth 2.0 authorization code + PKCE, in your browser
otl auth login --client-id <id>        # when an admin pre-registered the application
otl auth set-key < key.txt             # store an API key in the credential file (0600)
otl auth info                          # which credential is in use, and where it lives
otl auth logout                        # revoke and forget this profile's credentials
```

`otl auth login` discovers the instance's OAuth endpoints, registers `otl` as a public client if the instance allows it, and catches the redirect on a loopback port.
Tokens are renewed inside the request channel, so no command fails because a token aged out.

When several credentials exist for a profile, the order is OAuth session, then the credential file's API key, then `OUTLINE_API_KEY`.
Using the environment variable prints a one-time note; `OUTLINE_NO_KEY_WARNING=1` silences it.

TLS is required for every command unless the host is a loopback IP literal, credential-bearing requests never follow redirects, and a credential is only ever sent to the instance that issued it.

## Commands

Every command's `--help` ends in an `API contract:` section naming the operation it drives and a `JSON shape:` section stating what it writes to stdout.

```sh
otl docs search deploy                        # full-text search
otl docs list                                 # recently updated documents
otl docs list deploy --collection <id>        # same workflow, scoped
otl docs view <doc-id>                        # markdown; --raw to skip the pager, --web to open a browser
otl docs view <doc-id> --outline              # heading tree, byte sizes, and the revision
otl docs view <doc-id> --section 'Deploy'     # one section's markdown

cat notes.md | otl docs create --title Notes --collection <collection-id>
otl docs create --title Notes --collection <collection-id> --file notes.md
otl docs update <doc-id> --title "New title"
cat revised.md | otl docs update <doc-id>
printf '\nMore notes\n' | otl docs update <doc-id> --mode append
printf 'new wording' | otl docs update <doc-id> --mode patch --find-text 'old wording'
otl docs update <doc-id> --section 'Deploy' --file section.md --if-revision 12
otl docs update <doc-id> --delete-section 'Deploy > Rollback'
otl docs move <doc-id> --parent <parent-id> --index 0
otl docs delete <doc-id>                      # trash; --archive to archive instead
otl docs export --collection <collection-id> --out ./backup
otl docs update --file ./backup/Design.md     # write an exported file back

otl collections list                          # name / id / document count, every page fetched
otl collections create --name Engineering --icon 🛠️ --color '#3366FF'
otl collections update <id> --description 'Team knowledge base'
otl collections delete <id> --archive

otl comments list --document <id> --status unresolved
otl comments create --document <id> --text 'Looks good'
otl comments update <id> --resolve
otl comments delete <id>

otl fetch document https://outline.example.com/doc/runbook-abc123
otl fetch collection <collection-id>          # metadata plus full document tree
otl fetch user current_user
otl fetch attachment <attachment-id>          # short-lived signed download URL

otl attachments create --name image.png --content-type image/png --size 12345
otl users list --status active --role member --query jane

otl doctor                                    # credentials, instance, spec drift - one report
otl doctor --offline                          # local state only; contacts nothing
otl spec sync                                 # pull a newer spec when upstream adds an endpoint
otl spec reset                                # go back to the spec built into this binary
otl completions zsh > ~/.zfunc/_otl           # bash, zsh, fish, powershell, elvish
otl skill install                             # install the bundled agent skill
```

Six things worth knowing before you write a script:

- **`docs view` is markdown-first.** A pipe gets the document body, not JSON — the body *is* the data here. Ask for `--json` to get the document object instead. `--outline` is the exception: its datum is structure, so a pipe gets JSON.
- **`docs list` returns two shapes**, because it dispatches to two operations. Without a query, `documents.list` rows (`.[].id`); with one, `documents.search` hits, where the document is nested (`.[].document.id`). Reach for `docs search` whenever there is a query.
- **`docs create` and `docs update --json` return a receipt, not the document.** Identity fields only (`id`, `title`, `url`, `urlId`, `revision`, timestamps, …) — the body is not echoed back, so appending one line to a large page does not hand you the whole page. Read it back with `docs view <id> --json` when you need it.
- **`--section` edits one section without you handling the rest.** The CLI reads the body, splices it, and derives a `findText` that occurs exactly once, so only the changed part is sent and no repeated heading can be patched by mistake. A section runs to the next heading of the same or a higher level, so it includes what is nested under it. Address it by title, by parent (`'Deploy > Rollback'`), or with the level pinned (`'## Deploy'`); an ambiguous address is refused with every match listed.
- **Exported files name their document.** Each one opens with a YAML block carrying `outline_id`, `outline_url_id`, `title`, `revision` and `updated_at` — a file name is a sanitized derivative of a title, so without it an export is a copy you can read but never write back. `docs create --file` and `docs update --file` strip the block before sending; `docs update` also takes the id from it, making the ID argument optional. `docs export --no-front-matter` writes plain markdown instead.
- **Pagination never truncates silently.** `--limit N` is a cap you asked for: it warns and exits 0. The CLI's own page cap stopping a fetch early is not, and exits **9**.

Any write can be pinned with `--if-revision <n>` — the number `docs view --outline` reports — and is refused, before anything is sent, if the document has moved on. That check is this CLI's own on every mode; the revision also travels as `lastRevision` for instances that enforce it server-side, but nothing depends on their doing so. Section edits and `--mode patch` pin themselves to the revision they read, so the only gap left is the one between *your* read and your write, which is what the flag closes. Writing back an exported file closes it too: the block's `revision` becomes the pin unless you pass `--if-revision` yourself. `--force` drops it.

`--json` is the default whenever stdout is not a terminal, and it round-trips what the server sent.

## Configuration

Three layers, resolved **flag > environment > user config file, key by key**:

```toml
# config.toml in your config directory (~/.config/outline-cli on Linux/macOS,
# %APPDATA%\outline-cli\config on Windows). `otl --config FILE` overrides it.
default_profile = "work"

[profiles.work]
url = "https://outline.example.com"
auth = "api-key"                       # or "oauth", for `otl auth login`

[profiles.personal]
url = "https://notes.example.net"
```

```sh
otl --profile personal docs list       # or: OUTLINE_PROFILE=personal
otl --url https://other.example.com api auth.info
OUTLINE_CONFIG= otl api auth.info      # empty value: ignore the config file entirely
```

Credentials are scoped to their instance, so a profile reads its key from its own variable and nowhere else:

```sh
export OUTLINE_API_KEY=...             # used only when no profile is in effect
export OUTLINE_API_KEY_WORK=...        # used by --profile work
```

The name is `OUTLINE_API_KEY_` plus the profile name upper-cased, with anything other than an ASCII letter or digit becoming `_`.
A profile never falls back to the global variable — that would send one workspace's credential to another workspace's server.

The config file holds no secrets by construction: an `api_key` or `token` key is a hard error pointing at `credentials.toml`.

## Exit codes

<!-- BEGIN GENERATED EXIT CODES: regenerate with `UPDATE_EXIT_CODE_TABLES=1 cargo test -p outline-cli --test exit_code_tables` -->

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Generic failure |
| 2 | Usage or configuration error |
| 3 | API request rejected |
| 4 | Authentication or permission error |
| 5 | Resource not found |
| 6 | Server error |
| 7 | Network error |
| 8 | Rate limited |
| 9 | Partial failure |

<!-- END GENERATED EXIT CODES -->

Which errors map to which code is in [docs/exit-codes.md](docs/exit-codes.md), the source of truth this table is checked against.
A closed stdout pipe is not a failure: `otl ... | head -1` exits **0**.

In JSON mode the failure is also an object on **stderr**, while stdout stays empty:

```json
{ "error": { "exit_code": 2, "code": "usage", "message": "OUTLINE_URL is not set.\n..." } }
```

Argument errors caught by the parser itself keep the prose form, as do warnings that do not end the command — so a caller must not assume the object is always there.
The exit code is the fact that always is.

## Development

```sh
cargo test                             # unit and integration tests
cargo clippy --all-targets --all-features
scripts/win-check.sh                   # cross-compile lint: catches cfg-only dead code on Windows
```

## License

MIT
