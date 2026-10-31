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

Open `http://localhost:8080`. Create an invitation for a username:
```sh
./target/release/amalthea user create zeus
```

Open the generated registration link to choose a password and create the account.
Invitations are single-use. There are no administrator accounts or email addresses.

To let the recipient choose their username instead:
```sh
./target/release/amalthea user create --any-username
```

Other useful things:
```sh
./target/release/amalthea invite list
./target/release/amalthea invite url <ID> # retrieve URL
./target/release/amalthea invite revoke <ID>
./target/release/amalthea user list
./target/release/amalthea user quota zeus # show quota
./target/release/amalthea user quota zeus 2G # set quota
./target/release/amalthea user revoke-sessions zeus # sign out all devices
./target/release/amalthea --config /path/to/config.json server
```

Reducing a quota preserves existing files. New uploads and completion of pending
uploads are blocked when usage plus reservations exceeds the quota.

`max_files_per_user` defaults to 100 files including unfinished uploads and empty
files. `max_links_per_file` defaults to 10; expired/revoked links count until
deleted.

## Sharing and Transfers

- Create multiple independent sharing links per file, with optional expiry,
  revocation and access logs.
- Uploads reserve their full size and transfer in checksummed 4 MiB chunks.
  Pause/resume and per-transfer speed limits are available. After a browser
  restart, reselect the original file; its full SHA-256 must match.
- Resumable downloads store bytes and durable checkpoints in the browser's OPFS.
  Direct browser downloads are also available.

You *can* also manually move files in and out of the users directory,
the service handles this appropriately.  
**Respect your users privacy,** where not otherwise agreed.

## Setup and Deployment

Edit `config.json`, especially for the public `webroot`.
Relative paths are resolved beside the config file.
Data lives in the configured data dir, per user.
The SQLite database holds all the soft metadata.

*If you are forced to, make sure to edit and enable an appropriate imprint.*
The config's `imprint` points to an HTML fragment (e.g. `"imprint": "imprint.html"`).
Set it to `null` or omit it to display PRIVATE without an imprint link.

```sh
# From your development machine, connecting as the service user:
./setup.sh user@hostname /absolute/install/path

# On the server: edit /absolute/install/path/config.json, then:
systemctl --user start amalthea
# Publish the service via nginx (example in `ops/nginx.conf.example`).

# From your development machine, to update the installation:
./deploy.sh user@hostname /absolute/install/path
```

Both commands require Git, SSH and SCP locally. They transfer an allowlisted
`git archive HEAD` snapshot

Setup creates a private `config.json` from the example only if it is absent,
installs and enables the user service, and leaves it stopped for configuration.
Use deploy for subsequent updates. Both commands build on the server with
`cargo build --release --locked`, reusing the install directory's `target` cache.

## AI Disclosure

This project is almost 100% made by AI-assisted development tools.
In particular GPT-6-Astra and GPT-6-Sol.

Care has been taken to achieve a uniform and easily usable UI.
As well as a nice looking favicon + logo, and a (mostly) hand written readme file.

Absolutely no care has been taken to make the underlying code of the frontend
and backend look good or be well architected.
This works, that's good enough for me.
