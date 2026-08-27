---
name: outline-cli
description: Work with an Outline knowledge base from the shell using the `otl` CLI - search, read, create, update and export documents and collections, call any Outline API operation by name, and discover each operation's request and response contract offline. Use when the user mentions Outline, a wiki/knowledge-base document, an Outline URL or document id, or when `otl` is installed and the task involves reading or writing docs. Also use when `otl` reports an authentication, configuration or exit-code failure and the user needs to be guided through authorization.
version: 1.0.0
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
   explicitly unstable, the six curated commands are semver-stable.
3. **Never read, print, echo or copy a credential.** Do not open
   `credentials.toml`, do not paste an API key into a command line, do not put
   one in a file you create. `otl` reads secrets itself; `otl auth set-key`
   takes the key on **stdin**. Every `otl` surface (including `doctor` and
   `auth info`) is built to never print one back.
4. **Check the exit code**, not the wording. The codes are a published
   contract (table at the end). Code 9 means *partial* - the output is real
   but incomplete; never report it as success.
5. **Ask the user for anything interactive.** `otl auth login` opens a browser
   and waits for a redirect. Prefer `otl auth login --no-browser` and hand the
   printed URL to the user rather than trying to complete a consent screen.
6. **Do not retry rate limits by hand.** The CLI backs off on 429 internally;
   exit code 8 means the budget was already exhausted.
7. **Discover contracts, do not guess field names.** `otl api describe
   <operation> --json` is the single machine-readable contract for both `otl
   api` and the curated commands. Read it instead of assuming a JSON path.

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
```

## 3. The six stable commands

Their flags and output are a semver contract. Every one names its underlying
operation in `--help`.

```sh
otl collections list                              # id, name, document count
otl collections list --no-counts                  # skip one request per collection

otl docs search "deploy runbook"                  # full-text search
otl docs search "runbook" --collection <ID> --limit 20

otl docs view <ID>                                # markdown to a pager
otl docs view <ID> --raw                          # straight to stdout
otl docs view <ID> --web                          # open in a browser

otl docs create --title "Notes" --collection <ID> < body.md
otl docs create --title "Notes" --file body.md --draft
otl docs update <ID> --title "New title" --file body.md --publish

otl docs export --collection <ID> --out ./backup  # one markdown file per document
otl docs export --collection <ID> --out ./backup --overwrite --limit 100
```

Document ids: a UUID, or the short `urlId` from a document URL. Both work
wherever `<ID>` appears.

`--limit N` is a truncation you asked for: it warns on stderr and exits **0**.
The CLI's own pagination cap being reached is different - that exits **9**.

`otl docs export --json` summarizes with `complete`, `limit_reached`,
`enumeration_truncated` and `durable`. The test for "this backup is usable" is
`complete == true && durable != false` - `durable: null` means "this platform
cannot confirm a flush", not "it failed".

## 4. Any operation, and its contract

```sh
otl api list                                  # every operation, with a `callable` flag
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
  "parameters": [{"name": "query", "type": "string", "required": false, "description": "..."}],
  "response_fields": [{"name": "document", "type": "json", "container": "object",
    "fields_omitted": false, "fields": [
      {"name": "id", "type": "string", "container": null, "format": "uuid", "fields": []}
  ]}]
}
```

Read it like this:

- `parameters[].required` is often `false` even when one of several is needed -
  the `description` is where that is stated, so read the prose.
- `response_fields` is **recursive**. `container` is `object`, `array`, `union`
  or absent (a scalar). Children of an `array` describe **one item**. A `union`
  field carries no children on purpose: the alternatives are not one guaranteed
  shape, so no path is promised.
- `"fields_omitted": true` means this field HAS properties that are not listed
  here - a model that repeats one of its own ancestors (there is no finite
  expansion) or a field at the depth limit. Do not read `"fields": []` on such
  a field as "no properties": look at the ancestor of the same shape instead.
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

Curated commands keep the operation's own item shape in `--json`, so the
contract above is also their output contract.

## 5. Keeping the operation table current

```sh
otl spec sync            # fetch the upstream API description, compile, use it now
otl spec sync --url <U>  # a mirror or an internal copy
otl spec reset           # go back to the table built into the binary
```

Nothing here happens on its own: `otl` performs no background fetch and no
update check. `otl doctor`'s `online-spec` check is what reports drift, and it
only runs when you type it.

## 6. This skill

```sh
otl skill install        # install or upgrade this document for the agents on this machine
otl skill install --dir <SKILLS_ROOT>   # a specific skills directory
otl skill show           # print this document to stdout
```

`otl doctor` reports the `skill` check, and it never blocks. `ok` means every
installed copy matches this binary - or that none is installed, which is not a
fault. `warn` means a copy is out of step: behind, edited locally, declaring no
version, another skill occupying the path (`otl skill install --force`
replaces it), or a path that cannot hold a copy. Each entry in
`installed[]` carries its own `state` and `remedy`, so act on that rather than
on the summary.

## 7. Global flags, and the rest of the surface

Every command accepts these, and they outrank the environment, which outranks
the config file, key by key:

```sh
--json                # force JSON (already the default when stdout is not a terminal)
--profile <NAME>      # OUTLINE_PROFILE
--url <URL>           # OUTLINE_URL; on the same command line it deliberately redirects a profile
--config <FILE>       # OUTLINE_CONFIG; an empty value means "read no config file at all"
```

The commands not covered above:

```sh
otl completions zsh > ~/.zfunc/_otl   # bash, zsh, fish, powershell, elvish
otl auth info --offline               # stored state only
otl --version
```

`otl <command> --help` is authoritative for flags, and `otl api <operation>
--help` prints that operation's contract rather than generic help.

## 8. Exit codes (published contract)

| Code | Meaning |
|---|---|
| 0 | Success. Also a closed stdout pipe (`otl ... \| head`), which is normal completion |
| 1 | Generic failure: a response that is not JSON, an internal error |
| 2 | Usage or configuration error: bad flag, unknown operation, missing `OUTLINE_URL`, local validation failure, config-file problem, credential file permissions too open, plaintext `http://` |
| 3 | API request rejected (4xx that is not auth, not-found or exhausted rate limits) |
| 4 | Authentication or permission error: authenticate again |
| 5 | Resource not found (404) |
| 6 | Server error (5xx) |
| 7 | Network error: the request may never have arrived |
| 8 | Rate limited until the retry budget was exhausted; retry later |
| 9 | Partial failure: what you got is real, and some of it is missing |

Full text, including every case that maps to each code:
`docs/exit-codes.md` in the `otl` repository.
