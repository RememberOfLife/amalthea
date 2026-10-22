# Live API and transfer protocol

The API is deliberately unstable and mounted at `<webroot>/api/live/`. JSON
errors have an `error` string. Non-success responses must not be treated as an
upload/download acknowledgment. Mutations require `X-Amalthea: 1`; browsers must
also use the configured same origin. JSON requests use `Content-Type:
application/json`. There is no CORS API.

Authentication uses an opaque, HttpOnly, SameSite=Strict session cookie, Secure
when `webroot` is HTTPS. Credentials are Argon2id hashes. Invitation, session and
sharing tokens contain 512 random bits encoded as unpadded base64url; only SHA-256
token hashes are stored. Invitations are sent in the registration page's fragment
and submitted via JSON. Login failures are limited per username, and expensive
password operations have a concurrency cap.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/register` | `{token, username, password}`; consumes invitation and signs in. |
| POST | `/login` | `{username, password}`; signs in. |
| POST | `/logout` | Invalidates the current session. |
| GET | `/me` | Current user, quota, used/reserved bytes, chunk size. |
| GET | `/files` | Reconciled owner-only file list. |
| GET | `/files/:id` | Owner-only metadata, including size and SHA-256. |
| PATCH | `/files/:id` | Rename with `{name}`; never overwrites a destination. |
| DELETE | `/files/:id` | Permanently removes data, links and access records. |
| GET/HEAD | `/files/:id/content` | Authenticated download with Range/ETag. |
| GET/POST | `/files/:id/links` | List links or create `{label, expires_at}` (Unix seconds or null). |
| DELETE | `/links/:id` | Revoke one link, preserving its access log. |
| GET | `/links/:id/logs` | Latest 200 recorded requests. |
| GET/HEAD | `/public/:token` | Shared file metadata; does not expose owner or folder. |
| GET/HEAD | `/public/:token/content` | Shared download; checks expiry/revocation each request. |
| GET/POST | `/uploads` | List incomplete uploads or reserve `{name, size, sha256}`. |
| GET/PUT/DELETE | `/uploads/:id` | Status, append chunk, or cancel and release quota. |
| POST | `/uploads/:id/finish` | Verify full hash and publish; safe to retry. |

Owner file metadata includes `incomplete: true` when an imported filename violates
the filename rules. Such files count toward quota and retain their original disk
names until an explicit `PATCH /files/:id` supplies a valid name; successful repair
keeps the ID and sets `incomplete: false`. `DELETE /files/:id` is also available.
Downloading or creating links before repair returns `409`. Non-UTF-8 names are
displayed lossily; always address files by ID.

## Uploads

Compute the full source SHA-256 before starting. POSTing the same pending filename,
size and hash returns the existing upload. A different file with the same name
is rejected. The returned offset is authoritative; partial disk bytes beyond that
offset are truncated before the next write. The upload reservation is persistent.

PUT raw bytes (1 byte through 4 MiB) with `Upload-Offset` (decimal) and
`Upload-Checksum` (lowercase hexadecimal SHA-256 of this chunk). The server checks
the offset, remaining length and checksum; flushes bytes; then commits the new
offset in SQLite. If a response is lost, GET status before retrying. Out-of-order
or duplicate chunks return 409 rather than silently appending twice.

After all bytes arrive, POST finish. The server hashes the entire temporary file,
checks quota, and publishes without overwriting existing content. Empty files
skip PUT and go straight to finish. A lost completion response can be retried;
the file retains the upload ID. Uploads invisible to the file list never have
sharing links. A full hash mismatch requires canceling and restarting the upload.

## Downloads

Use the metadata's quoted SHA-256 as `If-Match`. A mismatch returns 412. Single
byte ranges (`bytes=start-end`, open-ended or suffix) return 206 with exact
`Content-Range`, `Content-Length`, `Accept-Ranges: bytes` and ETag. Malformed,
multiple or unsatisfiable ranges return 416 with `Content-Range: bytes */size`.
A stale `If-Range` returns the complete representation. All content is an
`application/octet-stream` attachment with a validated filename.

The browser requests bounded ranges, validates their headers and sizes, writes
them to OPFS, flushes data and appends a durable checkpoint. A crash can leave an
incomplete last chunk; resume truncates it to the last valid checkpoint. The
completed local file must match the full SHA-256 before Save is offered. Pausing
aborts the active fetch and leaves committed bytes available for resumption.
