---
name: outline-cli
description: Work with an Outline knowledge base from the shell using the `otl` CLI - search, read, create, update and export documents and collections, call any Outline API operation by name, and discover each operation's request and response contract offline. Use when the user mentions Outline, a wiki/knowledge-base document, an Outline URL or document id, or when `otl` is installed and the task involves reading or writing docs. Also use when `otl` reports an authentication, configuration or exit-code failure and the user needs to be guided through authorization.
version: 1.3.0
metadata:
  produced_by: outline-cli (otl)
  install: otl skill install
  upgrade_check: otl doctor
---

# Outline CLI (`otl`)

`otl` is a single binary that talks to one Outline instance. Everything below is
offline-discoverable: the operation table is compiled into the binary, so
`otl api list`, `otl api describe` and every `--help` answer without a network
request and without credentials.

This document ships inside the binary. `otl doctor` compares its version with
the copy installed here and says when to re-run `otl skill install`.

## Rules for an agent

1. **Diagnose before you act.** `otl doctor --offline --json` answers "is this
   environment usable" without contacting anything. Do it before blaming a
   command.
2. **Parse `--json`, never the table.** `--json` is already the default
   whenever stdout is not a terminal, so piping gives you JSON. The table
   layout is for humans and is not a contract; `otl api` output shape is
   explicitly unstable, while curated commands are semver-stable.
3. **Read the shape before you write the jq.** `otl <command> --help` ends in
   a `JSON shape:` section that states exactly what that command prints. It
   is not always the operation's own object, and one command (`otl docs
   list`) has two shapes. Section 5 below is the summary; the help is the
   authority.
4. **Never read, print, echo or copy a credential.** Do not open
   `credentials.toml`, do not paste an API key into a command line, do not put
   one in a file you create. `otl` reads secrets itself; `otl auth set-key`
   takes the key on **stdin**. Every `otl` surface (including `doctor` and
   `auth info`) is built to never print one back.
5. **Check the exit code**, not the wording. The codes are a published
   contract (table at the end). In `--json` mode the failure is also an
   object on **stderr** - see section 9. Code 9 means *partial*: the output
   is real but incomplete; never report it as success.
6. **Ask the user for anything interactive.** `otl auth login` opens a browser
   and waits for a redirect. Prefer `otl auth login --no-browser` and hand the
   printed URL to the user rather than trying to complete a consent screen.
7. **Do not retry rate limits by hand.** The CLI backs off on 429 internally;
   exit code 8 means the budget was already exhausted.
8. **Discover contracts, do not guess field names.** `otl api describe
   <operation> --json` is the single machine-readable contract for both `otl
   api` and the curated commands. Read it instead of assuming a JSON path.
9. **Prefer the curated command when there is one.** `otl api list --json` and
   `otl api describe --json` both carry a `curated_command` field naming it.
   Those commands are semver-stable; `otl api` output is not.

## 1. Is this environment usable?

```sh
otl doctor --offline --json     # local state only: nothing is contacted
otl doctor --json               # adds one auth.info probe and an upstream comparison
otl auth info --offline --json  # which credential is in use, and where it is stored
```

`doctor --json` returns `{healthy, problems, warnings, exit_code, checks[]}`.
Each entry in `checks[]` has `check`, `status` (`ok` / `warn` / `problem` /
`skipped`), `summary`, `detail[]` and its own facts. The check keys, in the
order they are reported:

| check | question |
|---|---|
| `configuration` | which config file and profile are in effect |
| `instance` | which base URL a request would go to |
| `credentials` | where the credential file is, and whether its permissions are sound |
| `credential` | which credential a request would actually send |
| `connectivity` | whether the instance answers (skipped with `--offline`) |
| `local-spec` | which operation table this binary dispatches from |
| `online-spec` | how that table differs from the online API description |
| `skill` | whether this skill is installed here and matches the binary |

`status: "warn"` never blocks: the environment works. Only `problem` does, and
the first `problem` in that order is the one to fix - everything after it was
measured in a broken environment.

`otl auth info --offline --json` answers the credential half alone; `otl auth
info --help` lists every field it returns.

## 2. Authorization, driven by what is actually on this machine

Run the two commands above first, then match the finding. Never invent a state:
every row below is decided by a fact in the JSON.

**A. `instance` is a problem / `auth info` shows `"instance": null`.**
No base URL. Two ways in, and the config file is the durable one:

```sh
export OUTLINE_URL=https://docs.example.com          # this shell only
```

```toml
# config.toml in the config directory reported as `config_file` by doctor
# (~/.config/outline-cli/config.toml on Linux and macOS,
#  %APPDATA%\outline-cli\config on Windows)
default_profile = "work"

[profiles.work]
url = "https://outline.example.com"
auth = "api-key"        # or "oauth", for `otl auth login`
```

The config file holds **no secrets**: an `api_key` or `token` key in it is a
hard error. Plaintext `http://` is refused for every command unless the host is
a loopback IP literal (`127.0.0.1`; the name `localhost` does not qualify).

**B. `credential` says no credential is configured.** Choose by what the
instance supports and what the user prefers:

```sh
# API key: Settings -> API in Outline. Read from stdin, never from argv.
printf %s "$KEY" | otl auth set-key --profile work
otl auth set-key            # or interactively, when a human is at the terminal

# OAuth (authorization code + PKCE), stored and refreshed by otl:
otl auth login --no-browser # prints the URL to open; --profile scopes it
otl auth login --client-id <ID>   # when an admin registered the application
otl auth login --scope "read"     # default is "read write"
otl auth login --timeout 300      # seconds to wait for the redirect
```

An API key can also stay in the environment, and is then scoped to the profile
it belongs to:

```sh
export OUTLINE_API_KEY=...        # used only when no profile is in effect
export OUTLINE_API_KEY_WORK=...   # used by --profile work
```

The variable name is `OUTLINE_API_KEY_` + the profile name upper-cased, with
anything other than an ASCII letter or digit becoming `_` (`self-hosted` ->
`OUTLINE_API_KEY_SELF_HOSTED`). A profile **never** falls back to the global
`OUTLINE_API_KEY`, so that one workspace's key cannot be sent to another
workspace's server. When a profile's variable is missing, `otl` names the exact
variable and exits 2 without sending anything.

**C. `credentials` is a problem: the file's permissions are too open.** The
credential file must be owner-only:

```sh
chmod 600 "$(otl auth info --offline --json | jq -r .credential_file)"
```

A world-writable *directory* around a sound `0600` file is a warning only.

**D. `plaintext_key_in_environment` is `true`.** Report it: the key is visible
to anything that can read the process environment. Suggest `otl auth set-key`
(file, `0600`) or `otl auth login` instead. Do not print the value.

**E. Exit code 4 from any command.** The instance rejected the credential -
expired key, revoked session, refresh no longer possible. `otl auth login`
again, or store a new API key. Exit code **2** is the other half of that split:
something to fix locally (a permission bit, a missing variable, a client id an
administrator must create).

**F. A stored credential belongs to another instance.** Pointing
`OUTLINE_URL` at a different instance while a profile is in effect exits 2 and
sends nothing. `--url` on the same command line is the deliberate way to
redirect a profile; otherwise switch profile.

Cleanup, when asked for it:

```sh
otl auth logout                  # forget this profile's credentials, revoke on the server
otl auth logout --purge          # also delete the application otl registered for itself
otl auth logout --force          # discard locally even if the server could not be told
```

`logout` exits 9 when a server-side step did not happen. By default it then
KEEPS the local credentials, because they are the only thing that makes a retry
possible; `--force` is the opt-out and means "these tokens stay live until they
expire".

## 3. Stable Outline workflows

Their flags and output are a semver contract. Every one names its underlying
operation and its JSON shape in `--help`.

```sh
otl collections list                              # id and name (see section 5 on counts)
otl collections list --query "eng" --limit 20
otl collections list --no-counts                  # skip one request per collection

otl docs search "deploy runbook"                  # full-text search
otl docs search "runbook" --collection <ID> --limit 20
otl docs list                                      # recent documents
otl docs list "runbook" --collection <ID>          # same as `docs search`, different JSON

otl docs view <ID>                                # markdown to a pager
otl docs view <ID> --raw                          # markdown straight to stdout
otl docs view <ID> --json                         # the document object instead
otl docs view <ID> --web                          # open in a browser

otl docs view <ID> --outline                       # heading tree; see section 4
otl docs view <ID> --section 'Deploy'              # one section's markdown

otl docs create --title "Notes" --collection <ID> < body.md
otl docs create --title "Notes" --file body.md --draft --icon "📓"
otl docs update <ID> --title "New title" --file body.md --publish
otl docs update <ID> --clear-icon
printf '\nMore notes\n' | otl docs update <ID> --mode append   # or prepend
printf 'new wording' | otl docs update <ID> --mode patch --find-text 'old wording'
otl docs update <ID> --section 'Deploy' --file section.md --if-revision 12
otl docs update <ID> --delete-section 'Deploy > Rollback'
otl docs move <ID> --collection <ID> --parent <ID> --index 0
otl docs delete <ID>                               # trash
otl docs delete <ID> --archive                     # archive

otl docs export --collection <ID> --out ./backup  # one markdown file per document
otl docs export --collection <ID> --out ./backup --overwrite --limit 100
otl docs export --collection <ID> --out ./backup --no-front-matter
otl docs update --file ./backup/Design.md          # write an exported file back

otl fetch document <ID-or-URL>
otl fetch collection <ID-or-URL>                   # metadata + full tree
otl fetch user current_user                        # or self, me, or a user id
otl fetch attachment <ID-or-URL>                   # signed download URL

otl collections create --name "Engineering" --icon "🛠️" --color '#3366FF'
otl collections update <ID> --description "Team knowledge base"
otl collections delete <ID> --archive

otl comments list --document <ID> --status unresolved --offset 0
otl comments list --collection <ID>                 # one of --document/--collection required
otl comments create --document <ID> --text "Looks good"
otl comments create --document <ID> --text "typo" --anchor-text "recieve"
otl comments create --document <ID> --text "agreed" --parent <COMMENT-ID>
otl comments update <ID> --resolve                  # or --unresolve
otl comments delete <ID>

otl attachments create --name image.png --content-type image/png --size 12345
otl users list --status active --role member --query jane --limit 50
```

Document ids: a UUID, or the short `urlId` from a document URL. Both work
wherever `<ID>` appears.

`fetch` also accepts a full Outline URL and extracts its final identifier.
Collection fetches combine metadata with the complete navigation tree. An
attachment fetch returns a short-lived signed URL without forwarding the
Outline bearer credential to the storage host - fetch that URL with a plain
unauthenticated request.

`attachments create` only obtains the pre-signed upload inputs; upload the
bytes directly to the returned storage URL. Inline comments are made with
`--anchor-text` (plus `--anchor-prefix` / `--anchor-suffix` when the phrase
repeats); replies with `--parent`. For comment updates, `--text` creates plain
ProseMirror paragraphs and keeps Markdown punctuation literal; use `--data
FILE` when rich comment formatting must be preserved. A rejected
`--text`/`--data` request reports only its error code, because the server may
quote the body back - add `--show-server-message` when you need the server's
explanation of what it did not like.

`--limit N` is a truncation you asked for: it warns on stderr and exits **0**.
The CLI's own pagination cap being reached is different - that exits **9**.

## 4. Changing part of a document

**Do not read a whole page to change part of it, and do not send one back.**
Three commands cover the loop, and none of them puts the full body in your
context:

```sh
otl docs view <ID> --outline --json               # heading tree + .revision
otl docs view <ID> --section 'Deploy' --raw       # just that section
otl docs update <ID> --section 'Deploy' --file new.md --if-revision 12
otl docs update <ID> --delete-section 'Deploy > Rollback' --if-revision 12
```

`--outline --json` returns `{id, title, revision, updatedAt, bytes,
sections[]}`, each section carrying `{level, title, path, line, bytes}`.
`path` is the address every other flag here takes; `revision` is the value
for `--if-revision`; `bytes` is how much you chose not to read.

Only the changed part reaches the network: the CLI reads the body itself,
splices it, and derives a `findText` that occurs exactly once. You never
compute an anchor and never handle the rest of the page.

**Addresses** are a heading title, its parents (`'Deploy > Rollback'`), or a
pinned level (`'## Deploy'`). Matching is exact, then case-insensitive, and
never a substring. An address matching two headings exits **2** and lists
both with line numbers — retry with the parent, do not guess. An unknown
address exits 2 and lists the document's whole outline, so one retry is
enough.

**A section includes what is nested under it.** It runs to the next heading
of the same or a higher level, so replacing `## Deploy` also replaces the
`### Rollback` beneath it, and deleting it deletes both. The last section of
a page therefore runs to the end of the page.

**A replacement includes the heading line**, exactly as `--section` printed
it — that is what makes renaming a heading expressible. The blank line
before the next heading is preserved for you, so a body without a trailing
newline cannot weld two sections together.

**Pass `--if-revision <N>`** whenever the text you are sending was written
against something you read on an earlier turn. Without it the write is still
pinned to the revision the CLI just read, which closes its own read-to-write
window — but not yours: an edit computed from a copy you read three steps ago
can otherwise apply cleanly on top of someone else's change. Exit **2** means
your copy was stale (read the outline again and redo the edit); exit **3**
naming revision N means the document moved between the CLI's read and its
write (same remedy).

**`--mode patch` is the lower-level form** and verifies its anchor before
sending: a `--find-text` occurring twice or not at all exits 2, and the
refusal names each position and the section it falls in. Prefer `--section`,
which cannot have that problem.

Two things this does *not* save: `documents.info` has no field selection, so
the CLI fetches the whole body either way, and there is no section-level
endpoint to fetch instead. What these commands reduce is what **you** have to
hold, which is the cost that binds.

## 5. What each command actually prints

`otl <command> --help` ends in a `JSON shape:` section, and that is the
authority. Five shapes are worth knowing before you write a single `jq`,
because in each one the obvious guess is wrong.

**`otl docs list` has two shapes.** It dispatches to two operations:

```sh
otl docs list --json          # -> [ <document>, ... ]                .[0].id
otl docs list QUERY --json    # -> [ {context, ranking, document} ]   .[0].document.id
otl docs search QUERY --json  # -> identical to the second form
```

A path written for one silently yields `null` against the other. Use `otl docs
search` whenever there is a query, and keep `otl docs list` for the
query-less listing.

**`otl collections list --json` carries no document count.** The counts in the
human table are computed by the CLI from `collections.documents`, and the API
cannot confirm them, so the JSON is the raw `collections.list` rows and nothing
else. `--no-counts` therefore changes nothing in JSON mode. To count in a
script, walk the tree:

```sh
otl fetch collection <ID> --json | jq '[.documents | .. | .id? // empty] | length'
```

**`otl docs create` and `otl docs update --json` return a receipt, not the
document.** Outline answers a write with the stored document, body included;
the CLI reports only the identity fields, so appending one line to a large
page does not hand you the whole page back:

```sh
otl docs update <ID> --json   # -> { id, collectionId, parentDocumentId,
                              #      title, url, urlId, revision,
                              #      createdAt, updatedAt, publishedAt }
```

Fields the server did not send are absent. `.text` is never in it - when you
need the stored body, read it back with `otl docs view <ID> --json`, or call
`otl api documents.update id=<ID> ...`, which forwards the operation's own
response unfiltered.

**Some commands compose an object rather than returning the operation's own.**

```sh
otl fetch collection <ID> --json   # -> { collection, documents }
otl fetch attachment <ID> --json   # -> { id, signedUrl }
otl docs view <ID> --web --json    # -> { id, title, url }
otl comments update <ID> --text T --resolve --json   # -> { comment, status }
otl docs view <ID> --outline --json   # -> { id, title, revision, updatedAt,
                                      #      bytes, sections: [ { level,
                                      #      title, path, line, bytes } ] }
otl docs view <ID> --section 'H' --json   # -> { id, revision, path, level,
                                          #      line, bytes, text }
```

`--outline` is the one place `docs view` follows the ordinary dual-state rule
instead of this command's markdown-first one: its datum is structure, so a
pipe gets JSON. `--section` keeps the markdown-first rule, and **that form is
the byte-exact one** — the `--json` wrapper is scrubbed of terminal control
characters like every object this CLI authors. Use plain `--section` (or
`--raw`) when the bytes matter.

Everything else - `docs view --json`, `docs move`, `collections
create/update`, `comments list/create`, `users list`, `attachments create`,
`fetch document`, `fetch user` - returns the operation's own object or array,
verbatim. Delete commands return `{"success": true}`;
their `--archive` variants return the archived entity instead.

**Exported files name their document, and can be written back.** Every file
`otl docs export` writes opens with a YAML block:

```yaml
---
outline_id: "55baa74a-bad1-4b16-a0d0-ec103c656b8e"
outline_url_id: "engKBTOaWe"
title: "Billing dunning"
revision: 15
updated_at: "2026-08-27T16:17:58.967Z"
---
```

`otl docs create --file` and `otl docs update --file` strip that block before
sending, so it never becomes document text. `otl docs update` also reads it:
the ID argument is optional when `--file` names an exported file, and the
block's `revision` becomes the write's `--if-revision` unless you pass one, so
a copy the document has moved past is refused rather than written. `--force`
drops that pin. An ID argument that disagrees with the block is a usage error
(exit 2), never a silent choice between them. Fields the server did not send
are omitted. `--no-front-matter` exports plain markdown instead, at the cost of
not being able to write it back by id.

**`otl docs export --json` is a run summary, not documents.** The markdown
files are the output. The summary is:

```json
{ "out": "./backup", "complete": true, "enumeration_truncated": false,
  "limit_reached": false, "durable": true, "stray": [],
  "exported": ["Alpha.md", "Alpha/Beta.md"],
  "failed": [ { "id": "...", "label": "...", "reason": "..." } ] }
```

`exported` holds the paths written, relative to `out`.

The test for "this backup is usable" is `complete == true && durable != false`
- `durable: null` means "this platform cannot confirm a flush", not "it
failed". In `failed[]`, `id` is `null` for a listing row that never had one, so
retry the entries where `id != null` and report the rest.

## 6. Any operation, and its contract

```sh
otl api list                                  # every operation, with `callable` and `curated_command`
otl api describe documents.search --json      # one operation's full contract
otl api documents.info id=<ID>                # call it: key=value pairs
otl api documents.list --limit 50             # auto-pagination, capped
otl api documents.create --body @doc.json     # raw JSON body, sent verbatim
```

Arguments are `key=value`, coerced to the types the operation declares and
validated locally before anything is sent (`--no-validate` skips the facet
checks when the vendored table disagrees with your instance).

`otl api describe <operation> --json` returns:

```json
{
  "operation": "documents.search",
  "path": "/api/documents.search",
  "body_mode": "key_value",
  "callable": true,
  "paginates": true,
  "source": "built-in",
  "curated_command": "otl docs search",
  "parameters": [{"name": "query", "type": "string", "required": false, "description": "..."}],
  "response_fields": [{"name": "document", "type": "json", "container": "object",
    "fields_omitted": false, "fields": [
      {"name": "id", "type": "string", "container": null, "format": "uuid", "fields": []}
  ]}]
}
```

Read it like this:

- `curated_command` is the semver-stable command that covers this operation,
  or `null`. When it is not null, use it: `otl api` output is explicitly
  unstable. `otl api list --json` carries the same field, so one local call
  tells you which of the 116 operations already have a stable front door.
- `parameters[].required` is often `false` even when one of several is needed -
  the `description` is where that is stated, so read the prose.
- `response_fields` is **recursive**. `container` is `object`, `array`, `union`
  or absent (a scalar). Children of an `array` describe **one item**. A `union`
  field carries no children on purpose: the alternatives are not one guaranteed
  shape, so no path is promised.
- `"fields_omitted": true` means SOME of this field's properties are not listed
  here - a model that repeats one of its own ancestors (there is no finite
  expansion) or a field at the depth limit. Some, not all: the flag can be
  `true` on a field that also carries `fields`, which is a recursive model with
  extra properties of its own. Do not read `"fields": []` on such a field as
  "no properties": look at the ancestor of the same shape instead.
  `"fields_omitted": false` with an empty `fields` really is empty.
- `body_mode: "raw_json_only"` means flat `key=value` cannot express the body -
  use `--body @file.json`. `callable: false` means `otl api` will not send it.
- `source` is `built-in` or `synced`, i.e. which table answered.

That recursion is how you find a nested id instead of guessing:

```sh
otl api describe documents.search --json | jq '.response_fields[] | select(.name=="document") | .fields[].name'
otl docs search "runbook" --json | jq -r '.[0].document.id'
otl docs view "$(otl docs search "runbook" --json | jq -r '.[0].document.id')" --raw
```

A few operations a curated command drives are **absent from the published API
description** and exist only in the table built into this binary:
`collections.archive`, `comments.resolve`, `comments.unresolve`, and the
`parentCommentId` / `statusFilter` parameters of `comments.list`. The curated
commands always dispatch from the built-in definitions, so after an `otl spec
sync` those commands keep working while `otl api describe` - which reads the
EFFECTIVE table - can report them as unknown.

## 7. Keeping the operation table current

```sh
otl spec sync            # fetch the upstream API description, compile, use it now
otl spec sync --url <U>  # a mirror or an internal copy
otl spec sync --spec <F> # compile a local OpenAPI file instead of fetching
otl spec sync --force    # rewrite the cache even when nothing changed
otl spec reset           # go back to the table built into the binary
```

Nothing here happens on its own: `otl` performs no background fetch and no
update check. `otl doctor`'s `online-spec` check is what reports drift, and it
only runs when you type it.

## 8. This skill

```sh
otl skill install        # install or upgrade this document for the agents on this machine
otl skill install --dir <SKILLS_ROOT>   # a specific skills directory (OUTLINE_SKILL_DIR does the same)
otl skill install --force               # replace a SKILL.md belonging to another skill
otl skill show           # print this document to stdout
```

`otl doctor` reports the `skill` check, and it never blocks. `ok` means every
installed copy matches this binary - or that none is installed, which is not a
fault. `warn` means a copy is out of step: behind, edited locally, declaring no
version, another skill occupying the path, or a path that cannot hold a copy.
`skipped` means this machine has no agent skills directory at all, so there was
nothing to compare. Each entry in `installed[]` carries its own `state`
(`current`, `behind`, `edited`, `undeclared`, `absent`, `foreign`, `unusable`)
and its own `remedy`, so act on those rather than on the summary.

## 9. Global flags, failures, and the rest of the surface

Every command accepts these, and they outrank the environment, which outranks
the config file, key by key:

```sh
--json                # force JSON (already the default when stdout is not a terminal)
--profile <NAME>      # OUTLINE_PROFILE
--url <URL>           # OUTLINE_URL; on the same command line it deliberately redirects a profile
--config <FILE>       # OUTLINE_CONFIG; an empty value means "read no config file at all"
```

**Failures are structured too.** In JSON mode a terminating error is an object
on **stderr**, while stdout stays empty:

```json
{ "error": { "exit_code": 2, "code": "usage", "message": "OUTLINE_URL is not set.\n..." } }
```

`code` is the name of the same numeric class in the table below. Two things do
not follow this shape, so never assume the object is there: argument errors
caught by the argument parser itself (an unknown flag, a missing required
option) stay prose, and warnings that do not end the command - a truncation
notice, the plaintext-key notice - are prose on stderr as well. The exit code
is the fact that is always present.

The commands not covered above:

```sh
otl completions zsh > ~/.zfunc/_otl   # bash, zsh, fish, powershell, elvish
otl auth info --offline               # stored state only
otl --version
```

`otl <command> --help` is authoritative for flags and for JSON shapes, and
`otl api <operation> --help` prints that operation's contract rather than
generic help.

## 10. Exit codes (published contract)

<!-- BEGIN GENERATED EXIT CODES (from docs/exit-codes.md; see tests/exit_code_tables.rs) -->

| Code | Meaning |
|---|---|
| 0 | Success: also a closed stdout pipe (`otl ... \| head`), which is normal completion |
| 1 | Generic failure: a response that is not JSON, an internal error |
| 2 | Usage or configuration error: bad flag, unknown operation, missing `OUTLINE_URL`, local validation failure, config-file problem, credential file permissions too open, plaintext `http://` |
| 3 | API request rejected: a 4xx that is not auth, not-found or exhausted rate limits |
| 4 | Authentication or permission error: authenticate again |
| 5 | Resource not found: the document, collection or other resource does not exist |
| 6 | Server error: the instance failed to process the request |
| 7 | Network error: the request may never have arrived |
| 8 | Rate limited: the retry budget was exhausted, so retry later |
| 9 | Partial failure: what you got is real, and some of it is missing |

<!-- END GENERATED EXIT CODES -->

Full text, including every case that maps to each code:
`docs/exit-codes.md` in the `otl` repository.
