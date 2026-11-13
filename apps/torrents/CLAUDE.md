# Torrents — notes for working on this repo

- GTK4 (gtk4-rs 0.11) + Rust, no libadwaita. Nyx Design 2.0 (`use_design_2`, see
  `STYLE.md`): one surface, no hairlines; cards and spacing separate the parts. The
  unified header (`chrome::app_header`) has the app and the sidebar toggle, a
  search-the-sites field in the middle (hidden on Search), and the speeds, the slow-mode
  chip, Add and Settings at the end. No status bar, no sidebar head. The sidebar: Home and
  Search, then Transfers (accent), Saved searches, Categories and Tags in their theme
  sections; the icon rail shows a short bar in each group's colour. Home
  (`sections/home.rs`) is the dashboard: speeds, shared, free space, a speed graph from
  `live::speed_bars` (a day of ten-second averages), downloading, connection, finished. Numbers use tabular
  figures (`tnum`); mono is for links, paths and hashes. Pages have no title header. The settings pages
  (group `Settings` in `sections::all`) open in `settings_dialog.rs`, a card over the
  window with its own search over `widgets::SEARCH`; `navigate("speed")` etc. open it.
  The theme, widgets, settings card and window frame come from the `nyx-*` crates.
  Every colour is a `@theme_*` token; cairo drawing gets colours from `theme::palette()`.
- Engine: libtorrent-rasterbar 2.x through `engine/shim.{h,cpp}` and the cxx bridge in
  `src/engine/ffi.rs`. Only plain values cross it; torrents are keyed by
  `info_hashes().get_best()` in hex. Settings go over by libtorrent name
  (`setting_by_name`), built from prefs in `engine/settings.rs`. Build needs `boost` headers.
- `engine/mod.rs` runs its own thread that owns the session. The UI sends `Command`s, and
  the thread sends an `Update` about once a second (statuses, stats, events, and details
  for the selected torrent) over an async channel. The engine thread writes resume
  data (`resume/<id>.fastresume`), `queue` and `session.dat` itself.
  On quit, `live::shutdown` blocks until the resume data is saved (10 s cap).
- UI state beside libtorrent's (categories, tags, incomplete-folder target, first/last
  flag) is in `store.rs` (`torrents.json`, `categories.toml`), with debounced writes.
- Transfers table: one `TorrentObject` per torrent. `relayout` works out order and
  visibility, and pokes the GTK filter/sorter only when the order changes.
  GTK re-binds rows synchronously, so never hold a `STATE` borrow across store,
  filter, sorter, selection or column-visibility changes. `table.rs` is the same idea
  for the details and search tables.
- Search runs qBittorrent nova3 plugins with our own MIT shims (`search/*.py`, embedded
  and installed to `~/.local/share/torrents/search`). There's one `python3` process per
  engine, and results come back as JSON lines.
- Control (`src/control.rs`): `nyx_core::control` on the app's bus name, for Nyx Assistant: `torrents.list`, `add` (links only), `pause`, `resume`.
- Checks: `cargo clippy --all-targets -- -D warnings`, `cargo test`, and
  `cargo test -- --ignored` (seeds a file between two local sessions over 127.0.0.1:46881–2).
- Visual check: a test copy on a private virtual monitor with a scratch profile,
  `NYX_TORRENTS_APP_ID=io.github.design_nexus.nyx.TorrentsTest XDG_CONFIG_HOME=/tmp/c
  XDG_DATA_HOME=/tmp/d nyx-torrents --section speed` (a separate instance, so the open copy
  is untouched).
