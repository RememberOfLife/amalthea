# Included browser assets

No package manager or frontend build is used. These pinned upstream releases are
served locally. License texts are included alongside the files.

| Asset | Version | Source |
| --- | --- | --- |
| Bootstrap CSS and JS bundle | 5.3.8 | https://github.com/twbs/bootstrap/releases/download/v5.3.8/bootstrap-5.3.8-dist.zip |
| Bootstrap Icons CSS and fonts | 1.13.1 | https://github.com/twbs/icons/releases/download/v1.13.1/bootstrap-icons-1.13.1.zip |
| Solid reactive runtime, DOM runtime, native html tag | 1.9.9 | https://github.com/solidjs/solid/releases/tag/v1.9.9 |
| js-sha256 streaming hash | 0.11.1 | https://github.com/emn178/js-sha256/tree/v0.11.1 |

Bootstrap and Icons were extracted from the official release zips; the archives
and unused assets were removed. Bootstrap JavaScript is available for future UI
styling but currently not loaded: Solid handles interactions.

Solid's published `dist/solid.js`, `web/dist/web.js`, and `html/dist/html.js` were
retrieved as static files from the version-pinned jsDelivr distribution. Only bare
module import specifiers in `web.js` and `html.js` were changed to local relative
imports, so the browser uses one shared reactive runtime without an import map.
`solid-html-README.md` documents the tagged-template API and its accessor rules.

`sha256.js` is the upstream source file and is loaded only in the transfer worker;
it provides incremental hashing without holding a whole large file in memory.

`SHA256SUMS` records the included assets after the import-path edits. When updating,
replace pinned release files intentionally, retain licenses, update this table,
and regenerate the checksums. No remote script is fetched by a user's browser.
