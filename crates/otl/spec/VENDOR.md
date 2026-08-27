# Vendored OpenAPI spec

- File: `spec3.json`
- Source: https://raw.githubusercontent.com/outline/openapi/main/spec3.json
- Upstream repository: https://github.com/outline/openapi
- Upstream commit (latest touching `spec3.json` at vendor time): `5f5c0c667d536383ca9bde564a52860123e2c6bd` (2026-08-24T22:02:59Z)
- Vendored on: 2026-08-25
- Local modifications: application-API overlay described below

To refresh, re-download the file from the source URL above and update this record.
# Local application-API overlay

The vendored document carries small, contract-tested additions for API routes
that ship in Outline's application server but are currently absent or
incomplete in the community OpenAPI repository:

- `collections.archive`
- `comments.resolve` and `comments.unresolve`
- `comments.list.parentCommentId` and `comments.list.statusFilter`

The definitions are based on Outline's `server/routes/api` schemas. Stable
curated commands always select these vetted definitions, even when the generic
`otl api` surface is using an independently synced table. The synced table
itself remains an exact compilation of the document the user selected.
