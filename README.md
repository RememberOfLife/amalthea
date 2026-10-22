# Amalthea

A small, self-hosted file sharing server for Linux. Each account has a private,
flat file store with a quota. Files become shareable only when their owner creates
a secret link. Built with Rust/Axum, SQLite, SolidJS tagged HTML templates, and
Bootstrap. The browser assets are included: no npm, Node.js, frontend compiler, or
external CDN is needed to build or run it.

## Quick start

Requires a current stable Rust toolchain and a C compiler on Linux.

```sh
cp config.json.example config.json
cargo build --release --locked
./target/release/amalthea server
```

Open `http://localhost:8080`. In another terminal, create an invitation:

```sh
./target/release/amalthea user create
# Optional: 20 GiB quota, invitation valid for 24 hours
./target/release/amalthea user create --quota 20G --expires-hours 24
```

Open the printed registration link to choose a username and password. Invitations
are single-use. There are no administrator accounts or email addresses.

```sh
./target/release/amalthea user list
./target/release/amalthea user quota alice                 # show bytes and readable size
./target/release/amalthea user quota alice 50G             # set 50 GiB
./target/release/amalthea user quota alice 1.5G            # fractional sizes work
./target/release/amalthea user quota alice 53687091200     # raw bytes still work
./target/release/amalthea --config /path/to/config.json server
```

Size suffixes are case-insensitive binary units: `K`, `M`, `G`, `T`, `P`
(and `KiB`, `MiB`, etc.). Fractional bytes are rounded down. Quota and user-list
output includes both forms, e.g. `53687091200 (50 GiB)`. The configuration's
`default_quota_bytes` also accepts either a size string such as `"100G"` or a
numeric byte count such as `107374182400`.

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

## Configuration and deployment

Edit `config.json` for the public `webroot`, loopback `listen` address,
`data_dir`, quotas, and retention settings. Relative paths are resolved beside the
config file. Static resources live in `res/static/` and are served at
`<webroot>/res/static/…`. A URL prefix such as `https://example.org/files` is supported.

Data lives in `data/users/<username>/<filename>`. The SQLite database holds
accounts, file metadata, links, and access logs; `data/uploads/` holds partial
uploads. Regular files manually added or removed are reconciled on startup,
periodically, and when listing files. Replaced content gets a new identity and
loses its previous sharing links. Filenames accept ASCII letters, digits,
underscores, and dots, with no leading dot and at most 200 characters. Files with
invalid names appear as **Incomplete** until the owner explicitly uses **Fix name**;
they can also be deleted. No automatic renaming occurs. Subdirectories and symlinks
are excluded.

```sh
# On the server, as the unprivileged service user:
./setup.sh /absolute/install/path
# Edit the installed config.json, then:
systemctl --user start amalthea

# From your development machine, after initial setup:
./deploy.sh user@hostname /absolute/install/path
```

Setup installs a user systemd unit. Deploy copies an explicit source allowlist,
builds on the target, and restarts the service. Neither script needs root.
See [operations](docs/operations.md), the [nginx example](ops/nginx.conf.example),
and [API/transfer notes](docs/api.md). Setup and deployment scripts are not run
as part of development tests.

## Development

```sh
cargo check --locked
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

Backend tests keep their data under `target/test-data/` and do not install a
service. There are no frontend tests. Edit `res/static/src/main.js` (Solid `html` tagged
templates) and `res/static/src/style.css` directly; reload to see changes. Static vendor
versions, licenses, sources, and checksums are in [res/static/vendor](res/static/vendor/README.md).

V1 deliberately excludes folders, groups, SFTP, upload-only links, and recovery
or backup guarantees. Database migrations run automatically and are forward-only.

---

## Design & Architecture

Do not touch this section significantly, its the design record.
Please DO however make the rest of this readme, i.e. above, a proper small but useful readme for this to be an open source file sharing server...

Good error handling! Things that can error that can be checked beforehand ARE checked beforehand.

Within meta directory there exists an alternative old version of the 

### DevOps

We want a setup.sh and a deploy.sh script, we'll be adjusting that to work together but the rough frame should be there I guess.
Setup does initial setup for the server like the systemd unit file (which should be in a directory here too) and placing the stuff on the server.
User local (--user) systemd unit pls. Please make it agnostic to where the thing is on the server.
Setup is ran ON the server I guess?
Deploy is ran from another machine and gets the connectable hostname of a machine that should be deployed to this means placing code via scp and doing a build on the server and restarting the systemd unit (unless you have significantly other better ideas for what it should do).
Please write these two scripts without "testing" them, i.e. so you dont place infect or etc stuff allover my dev system here.

The server is living behind a nginx reverse proxy that handles SSL etc.

### Web Interface

We use handlebars templates (if and where necessary, maybe we dont need them? idk..).
Design is bootstrap. With bootstrap icons. Feel free to download the newest release zips for both, and emplace the bundle min.js and min.css and so on in e.g. a res directory or whatever is necessary.
We will be using solidjs for interactivity etc!

User can inspect storage quota and upload/download and set permissions / passwords / pass token in link etc access to their files.
Multi user. Each has their own permissions.
For NOW, no groups, but in the future...

Because I will want to style the web interface myself, please make it like.. minimal I guess?
I.e. do I can easily move stuff around and make it look how I want later on.
However, please DO make sure it work on mobile too!

UI philosophy wise, it should be powerful yes, but mostly really like, easily support the one and correct workflow for using it.
Such that only partially technical users can also easily use the service to upload and download and share.
But importantly such that it doesnt sacrifice ui or capability for powerusers.
Ideally we just dont have that many settings or toggles if its not really needed, or hide them where defaults are sane. etc etc.

#### Pretty URLs are important

E.g. a users base directory is:
`<webroot>/<username>`
im not sure if we want trailing slashes everywhere or nowhere, do the right thing whats sensible. probably no trailing slashes for files but always have for directories???
in any case when decided on it, the thing should auto correct urls to the correct pattern
i.e. if user types above it gets corrected to:
`<webroot>/<username>/`
now that is a directory navigatable etc etc.. the user upload stuff
the user can obviously make files available for non authenticated users.
links for these should not be *entirely* weird ids, so its at least a little sensibly human readable, but not like expose the users directory structure etc etc

### Server

Offers authenticated up and down via the API, accessible through the web interface.
Will offer sftp into the files, LATER on, so not now, but keep in mind for designing.

For a reference on rust code style and packages etc see:
`/home/urs/projects/webshop_printmaking/` and `/home/urs/projects/personalsite/`.
These two directories are EXPLICITLY safe to explore. But only within there, nothing else!

For configs see the config pattern in the ref repos listed.
E.g. for webroot and the actual data storage path where everything lives.

We will probably want to just use sqlite for the db, but ideally make it interchangable with a real MariaSQL thing, so dont depend TOO hard on sqlite,
and make sure theres proper facilities for db migration up and down from day one. DB migration is done by the application on initial startup, if necessary.

Important feature we absolutely want: a user can create an "upload link" for a directory(, or file? but idk why..) so another use can use the link to upload something to there. Of course with size constraints and usage limit or whatever??

### Browser

Firefox first. Chrome second, ideally without any explicit support other than where required to work.
Supports pausable, throttleable and resumable downloads.
I.e. using OPFS so it can resume if anything crashed.
And uploads in chunks so it can handle very very large files! (Think 16GB)
Upload also support pause, throttle and resume after interruption etc...

### Testing

If useful add tests for whatever works and or is sensible. But make sure the tests are all self contained here and do not install the service as a system resource or whatever etc etc..

### Decisions to clear it up.

v1 of this is complete when we have multi users that have their own accounts and data folders.
In v1 the users data directory does NOT support sub directories but is essentially just a flat list of the "stuff" they have there.
Features: User account self uploading, download links with secret embeded. ofc management of the list of files the user has..
Quotas resumable uploads downloads are all part of this too already.
Accounts are administrator only, through a registration link which can be used to create the account.
No emails. just username ids (A-Za-z0-9_) and passwords.
There are no administrator accounts, all accounts are the same in this version. To create a registration link just call
`amalthea user create` or similar. whereas the normal service server runs with `amalthea server`.
Permissions for now dont mean anything. because we only have per item stuff.
But files are only accessible via their token download thing when that was explicitly made by the user, we dont "default" create that stuff.
Secret tokens need to be like 512 bits I think and maybe just use base64.
In this version a file can have multiple independantly revocable links with their own expiration and access log (see if someone clicked or downloaded).
Upload link recipients can ONLY upload. I.e. not even download its own file, but is not supported in this v1 without subdirectories anyway so dont worry.
Resumptions should survive connection loss page reload browser restart and computer restart. i.e. use the file state to do this, not some cookie or whack state.
The site of course verifies what it is continuing makes sense and is not accidentally another file entirely etc etc..
users dont have nested folders here so just file renaming and deleting. deletion is permanent and immediate, no trash, but the deletion box required at least 2 clicks so no accidents.
W.r.t. storage and metadata, do whats sensible? Ideally the data folder directory content is more or less its own thing.
I.e. we restrict contents for the alphanum thing from above, with . too. thats it. then we can have our own metadata files for each file there?
if an admin deleted something the metadata file gets cleaned automatically (idk when but you go figure it out). and when they just add a file it gets a new metadata file autogenerated automatically (you decide when etc..)
auth is username/password only.
no stable api for now, so we use /api/live/ which is the LIVE version which is unstable but just like, the actual usage.
deploy is linux only, I guess itd be sensible to build the app on the deploy target? then we dont have to worry about cpu bullshit or crosscompiling etc etc.
no recovery guarantees for now, this is more or less an ephemeral file store. downward migrations arent really needed no.
