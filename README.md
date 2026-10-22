# Amalthea

Amalthea is a small self-hosted file sharing server, intended for private use
(such as family and friends), where a clean and simple web interface is required.

Features shortlist:
- Accounts with individual quotas and private storage.
- Access tracked shareable download links for files.
- Pause/Resumeable uploads/downloads even across tab-reloads.

## Quick Start

Requires a current stable Rust toolchain and a C compiler on Linux.
```sh
cp config.json.example config.json
cargo build --release --locked
./target/release/amalthea server
```

Open `http://localhost:8080`. To use, create a default invitation:
```sh
./target/release/amalthea user create
```

Open the generated registration link to register an account with username and password.
Invitations are single-use. There are no administrator accounts or email addresses.

Other useful things:
```sh
./target/release/amalthea user list
./target/release/amalthea user quota zeus # show quota
./target/release/amalthea user quota zeus 2G # set quota
./target/release/amalthea --config /path/to/config.json server
```

Reducing a quota preserves existing files. New uploads and completion of pending
uploads are blocked when usage plus reservations exceeds the quota.

## Sharing and transfers

- Upload, download, rename, and permanently delete files at `/<username>/`.
  Deletion requires a confirmation click.
- Create multiple independent sharing links per file, with optional expiry,
  revocation, and access logs. Copy each link when creating it: only its token
  hash is stored on the server. Renaming keeps links working.
- Uploads reserve their full size and transfer in checksummed 4 MiB chunks.
  Pause/resume and per-transfer speed limits are available. After a browser
  restart, reselect the original file; its full SHA-256 must match.
- Resumable downloads store bytes and durable checkpoints in the browser's OPFS.
  Resume after a reload/restart, then save the verified file. The browser-storage
  list includes downloads from all accounts/links and leftover files; remove them
  there to reclaim space. Saving a copy does not clear this storage. Browser storage
  limits apply; clearing site data or using private browsing can remove it.
  Direct browser downloads are also available.
- Files are checked with SHA-256 before publication and after resumable download.
  Transfers use bounded memory, including files larger than 16 GiB.

Use a current Firefox or Chrome over HTTPS; localhost HTTP also supports the
browser APIs. Restart recovery uses server files and browser disk storage, not
cookies. Login cookies authenticate requests; sign in again if one expires.

You *can* also manually move files in and out of the users directory,
the service handles this appropriately.  
**Respect your users privacy,** where not otherwise agreed.

## Setup and Deployment

Edit `config.json`, especially for the public `webroot`.
Relative paths are resolved beside the config file.
Data lives in the configured data dir, per user.
The SQLite database holds all the soft metadata.

```sh
# TODO On the server, as the service user:
./setup.sh /absolute/install/path
# Edit the installed config.json, then:
systemctl --user start amalthea
# Publish the service via nginx (example in `ops/nginx.conf.example`).

# From dev pc to a servers install installed:
./deploy.sh user@hostname /absolute/install/path
```
