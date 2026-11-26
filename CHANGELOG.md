# Changelog

Every release of the image-host plugin, newest first. Each version number matches
`version` in `config.json` and the git tag that produced it.

## [Unreleased] 0.7.1

Not tagged yet. The panel is split into pages, and HTTP Access no longer depends
on an app settings page that was removed. Nothing was removed and no control key
changed, so settings saved by 0.6.1 load unchanged.

**Pages**

- **Added** five pages, in this order: Upload, Storage & Link, Compression &
  Naming, HTTP Access, Advanced. Which control lives where:
  - *Upload* — remote picker, refresh, image path, Choose image…, Get direct
    link, Save settings
  - *Storage & Link* — storage folder, short-link template, direct link and its
    copy button, short link and its copy button, link hint
  - *Compression & Naming* — size limit, compress-if-oversized, deduplicate,
    dedupe hint
  - *HTTP Access* — the upload-service heading, Local port, upload URL, Detect
    port, hint
  - *Advanced* — Template 1 parameters (format, method, rate cap, JSONP callback,
    endpoint, api_key) and their hint
- The status line, progress bar and checks table stay shared: they carry no
  `page`, so they show on every page.
- Buttons that share a `line` stay on one page; a `line` group cannot span pages.
- Switching pages does not fire a plugin action and does not drop form input.
- A host older than 0.3.0 ignores `pages` and `page` and lays every control on one
  page. Nothing stops working.
- **Changed** Copy direct link / Copy short link sit on the same row as the field
  they copy, instead of below the whole form.
- **Changed** Detect port moved next to Upload URL, on the HTTP Access page.
- **Changed** Labels and hints are shorter throughout, in both languages.

**HTTP Access without the app's settings page**

- **Added** a `http_port` field (Local port). The port used to be readable only
  from the host's own `config.json`; when the app drops its HTTP settings
  section, `Detect port` found nothing, blanked the upload URL and sent you to a
  page that no longer exists. A port typed into this field now survives that and
  drives the address instead.
- **Fixed** `Detect port`: it keeps the port already in Local port when the host
  read fails, recomputes Upload URL from it, and says so in the status line
  instead of pointing at Settings. When the host read succeeds it still wins.
- The hint on HTTP Access states where the port comes from: this page, 5273 by
  default. The page only switches the upload service on and shows the address.

**Engineering**

- **Added** `CHANGELOG.md` and a CI workflow: format check, `cargo test`,
  `cargo check --target wasm32-wasip1`, `config.json` structure checks (page
  declared, `line` not spanning pages, unique keys, action labels) and a packaging
  check that the zip holds exactly `check.sha256`, `config.json` and the wasm
  entry.
- `release.yml` runs the same gates plus a cross-check of the tag, `config.json`,
  `Cargo.toml` and `Cargo.lock` versions before packing, only publishes on a tag,
  and fails if that release already exists instead of overwriting a published
  asset's sha256. A manual run only produces an artifact.
- **Packaging**: the build sets `--remap-path-prefix`, so the shipped wasm no
  longer embeds this machine's cargo path. Panic locations from dependencies used
  to carry the user name and directory layout into the artifact.

## 0.6.1 — 2026-10-26

- English strings shipped next to the Chinese originals; README rewritten to
  cover both.

## 0.6.0 — 2026-10-25

- HTTP upload service for external clients, with a size limit and JPEG
  re-encoding for oversized images.

## 0.5.0 — 2026-10-24

- Rewritten as a standalone plugin: upload, direct link, then optional short
  link.

## 0.4.2 — 2026-10-24

- First paint no longer replaces the configured remote with the first entry of
  the candidate list.

## 0.4.1 — 2026-10-24

- Remote list read from `rclone.remotes`, so an RC 401 no longer turns the list
  into an error; the remote `select` declares `dynamic`.

## 0.4.0 — 2026-10-24

- Panel back to the direct layout: storage remote, folder, upload.

## 0.3.0 — 2026-10-24

- First release as its own repository: rclone-backed upload, dedupe, index,
  direct link.
