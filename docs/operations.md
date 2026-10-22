# Operations

## Configuration

Start from `config.json.example`. Unknown keys and invalid URLs/limits are rejected.

| Key | Default | Meaning |
| --- | --- | --- |
| `webroot` | `http://localhost:8080` | External URL, with optional path prefix; used for links, cookie scope and origin checks. |
| `listen` | `127.0.0.1:8080` | Internal HTTP listener; put nginx in front for HTTPS. |
| `data_dir` | `data` | Private database, user files, and partial uploads. Must be outside `static_dir`. |
| `static_dir` | `res/static` | Included browser assets, served at `<webroot>/res/static/…`; no build step. |
| `loglevel` | `info` | Rust tracing filter. Avoid verbose third-party logging in production. |
| `default_quota_bytes` | `107374182400` | Default invitation quota (100 GiB). Accepts integer bytes or a size string such as `"100G"`. Per-account changes use `user quota`. |
| `session_days` | `30` | Fixed login lifetime. Logout immediately invalidates the session. |
| `upload_expiry_days` | `30` | Remove incomplete uploads inactive for this many days; release reservations. |
| `reconcile_seconds` | `60` | Interval for disk reconciliation and expired-record cleanup. |
| `access_log_days` | `90` | Retain sharing-link access records for this many days. |

All relative filesystem paths resolve against the configuration file's directory.
If upgrading from the earlier `web/` layout, change `static_dir` to `res/static`.
The process needs write access to its data directory and read access to static
assets. Newly created files use a private umask. One server process may use a data
directory at a time. CLI operations can run alongside it.

CLI `user quota` and `user create --quota` accept raw bytes or case-insensitive
binary suffixes: `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, through `P`/`PB`/`PiB`.
For example, `1.5G` is 1610612736 bytes. Fractions support up to 18 decimal
places; fractional bytes are rounded down. In configuration, `default_quota_bytes`
accepts either an integer byte count or the same size strings, for example
`"default_quota_bytes": "1.5G"` or `"default_quota_bytes": 1610612736`.
Quota and user-list output shows bytes followed by the readable size in
parentheses.

Changing a user's quota updates the database immediately. A zero quota is allowed.
Existing data remains downloadable. Pending uploads reserve their entire declared
size; canceling an upload releases that reservation. An already-reserved upload
can continue receiving chunks after a quota reduction, but cannot publish until
usage plus reservations fits the new quota. Disk-free-space checks also account
for outstanding upload reservations. They cannot prevent another process from
filling the disk later; failed writes remain retryable from the committed offset.

## Disk administration

The data layout is:

```text
data/
  amalthea.sqlite3          # metadata and credentials; keep private
  amalthea.sqlite3-wal      # SQLite-managed when present
  amalthea.sqlite3-shm      # SQLite-managed when present
  users/
    Alice/
      notes.txt
  uploads/
    <upload-id>            # unpublished partial data
```

Usernames are 1–64 ASCII letters, digits or underscores, unique case-insensitively.
A few route names (`api`, `assets`, `s`, `login`, `register`, `health`, `favicon`,
`index`, `res`) are reserved. Filenames have at most 200 ASCII characters, allow letters,
digits, underscores and dots, and cannot begin with a dot. Regular files with
invalid names appear as **Incomplete — needs a valid filename** and count toward
quota. They cannot be downloaded or shared until the owner explicitly chooses
**Fix name** and saves a valid name. **Delete** remains available. Import never
changes filenames automatically. Non-UTF-8 names are displayed with replacement
characters, but repair and deletion target the exact original filename by file ID.
Subdirectories and symlinks are not imported or served.

For manual import, finish writing the file elsewhere, then atomically move it
into the user's folder. Large imports are hashed before appearing in the list.
Manual imports count against quota, even if they exceed it. Missing files lose
their metadata and links; changed files get a new ID. Permissions/timestamps can
also invalidate a file's identity. Do not edit a file in place while it is being
downloaded. The application detects changes before streaming, but cannot freeze
external writes to an already-open file. Resumable browser downloads additionally
verify the entire result before offering it for saving.

Metadata mutations are serialized within the server for quota and filesystem
consistency. Full-file verification/import may delay other mutations while it
runs; existing download streams continue. A canceled HTTP completion request may
still finish on the server; refreshing reconciles the resulting file state.

This is an ephemeral store, not a backup service. To copy a consistent store,
stop the service and copy the whole data directory. Recreating only `users/`
will not restore accounts, invitations, or sharing links. Migrations are embedded
in the binary and run at startup/CLI initialization; do not downgrade a binary
against an unknown newer schema.

## HTTPS and service management

`setup.sh` runs on the target as its ordinary service user. It checks required
tools, copies the project when necessary, builds Rust, writes a location-specific
user unit, and enables it. It preserves an existing `config.json`. Edit that file
and start the unit explicitly. To keep it running after logout and at boot, the
machine's administrator may need to enable user lingering.

```sh
systemctl --user start amalthea
systemctl --user stop amalthea
systemctl --user restart amalthea
journalctl --user -u amalthea
```

`deploy.sh user@host /absolute/install/path` requires SSH/SCP, a Rust toolchain and
C compiler on the server, and a working user systemd manager. It stages and builds
before stopping the service, then replaces the binary and assets together. It
preserves the previous binary/assets for manual rollback, but migrations are not
rolled back. No live configuration or user data is copied from the developer's
machine. Deployment is Linux-only. The scripts are intentionally not exercised
by the test suite.

Use the nginx example and set `webroot` to the exact external HTTPS origin and
path. Preserve a configured URL prefix when proxying. The proxy body limit only
needs to exceed 4 MiB; whole files never pass through as one request. Allow long
response timeouts for verification of large files. No compression should be
applied to binary download responses.

Sharing URLs contain bearer secrets. Do not include them in proxy/access logs or
analytics. The example disables nginx access logging. Application access records
store event time and requested byte range, without IP addresses, tokens or user
agents. A view is a public metadata request; a download is a content request, not
proof that the recipient finished saving. HEAD requests are not logged. The UI
shows the latest 200 events per link; retention is configurable. Revocation blocks
subsequent requests, including resumed ranges, but cannot retract bytes already
sent or abort an existing stream.

The UI loads all assets locally. Solid's native tagged-template runtime requires
CSP `unsafe-eval` to create functions from developer-authored templates. User
values are bound as text/properties and are never used as template source or raw
HTML. Inline scripts, framing and cross-origin scripts remain disallowed.

## Browser storage

OPFS works on HTTPS and localhost. Download chunks are flushed to disk before a
checkpoint is committed. After a crash, uncommitted bytes are truncated before
resuming. Exclusive browser locks prevent two tabs from writing the same transfer.
The final file is hashed before it is offered for saving. Saving creates a normal
browser download from the disk-backed File; wait for that save to finish, then use
**Remove from browser** to reclaim its OPFS storage. Signing out does not delete already downloaded local
bytes. On a shared computer, discard transfers and clear site data when finished.

The browser may deny persistent storage or lack enough quota for a large file.
Private mode, clearing site data, or browser eviction can remove partial files.
Use direct download if OPFS cannot accommodate the file. Native direct-download
resume behavior is controlled by the browser; the server supports byte ranges.

The **Downloads stored in this browser** section is an inventory of the complete
OPFS transfer directory for this Amalthea URL prefix, including other accounts,
sharing links, incomplete manifests, and orphaned files. It is available even on
the sign-in page. Uploads are listed separately and do not copy source files into
OPFS. Per-download removal deletes data, metadata, and the checkpoint journal.
**Clear browser downloads** asks for confirmation and removes the whole transfer
directory; it requires downloads in every tab to be paused. Empty directories are
cleaned up too. A rescan confirms the result. Saved copies and server files are
not removed.

The inventory refreshes periodically, on window focus, after transfer changes,
and when another tab reports a change. Its timestamp shows when it was checked.
Files locked by a worker remain listed with an unknown-size indicator if the
browser cannot inspect them. Scan failures are explicit and never presented as
an empty store. The empty message concerns this application's transfer storage,
not browser caches, login cookies, or other applications sharing the origin.
