# Torrents

A BitTorrent client for NyxOS and [Omarchy](https://omarchy.org). It follows the
system theme and fits a half-screen tile.

## Features

- **A proven engine.** Torrents is built on
  [libtorrent](https://libtorrent.org) 2.x. It handles v1, v2 and hybrid torrents,
  magnet links, DHT, peer exchange, local peer discovery, µTP, encryption, UPnP and
  NAT-PMP, proxies, and resuming where it left off.
- **Your torrents at a glance.** Progress, status, speeds, ETA, seeds, peers, ratio,
  category and tags.
  - Filter from the sidebar by status, category or tag; each shows a live count.
  - Sort by any column, and hide the columns you don't need.
- **Details for the selected torrent.** A pieces map, trackers, connected peers, the
  files with their priorities, and a speed graph.
- **Adding torrents.** Drop a `.torrent` file or magnet link on the window, paste a
  link, or open one from your browser.
  - The add dialog sets the folder, category, tags and which files to download.
  - For magnet links, it fetches the file list before you decide.
- **Queue, categories and tags.** Limit how many torrents run at once and in which
  order. Categories can save to folders of their own.
- **Search.** Search many sites at once with community search plugins (the widely
  used nova3 format). Plugins can be installed from a file or a link, or imported
  from an existing qBittorrent install.
- **Speed limits.** Global limits, plus a slow mode you can switch on from the
  status bar or run on a schedule.
- **Seeding limits.** Stop at a share ratio or after a seeding time.
- **When a download finishes.** Get a notification, run a command, or move it out
  of an incomplete-downloads folder.
- **Stay on your VPN.** Bind Torrents to your VPN's network interface, and torrents
  stop whenever the VPN is down.

## Install

On NyxOS, choose Torrents in Welcome when you set up, or install it later from the Store or with `nyx install nyx-torrents`. On Omarchy:

```sh
curl -fsSL https://raw.githubusercontent.com/design-nexus/nyx-torrents/main/install.sh | bash -s -- torrents
```

This installs any missing build dependencies (`rust gtk4 libtorrent-rasterbar boost`),
builds the app, and puts `nyx-torrents` in `~/.local/bin` with a launcher entry. Add
`-s -- --default` after `bash` to also open magnet links and `.torrent` files with it.
Search plugins need `python3`, and adding links to `.torrent` files needs `curl`.

To remove it, run the same line with `uninstall.sh` in place of `install.sh`. Add
`-s -- --purge` to also delete its settings. Your downloads are never touched.

## Use

```sh
nyx-torrents                           # open the window, or bring it to the front
nyx-torrents file.torrent 'magnet:?…'  # add torrents to the open window
nyx-torrents --section search          # go straight to a page
nyx-torrents --quit                    # save and stop
```

Closing the window quits Torrents. To keep downloading in the background instead,
turn on **Keep running when closed** in Appearance.

| Keys | |
| --- | --- |
| <kbd>Ctrl</kbd>+<kbd>O</kbd> | Add a .torrent file |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Add a magnet link |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> | Filter the list |
| <kbd>F1</kbd> | Show the shortcuts |
| <kbd>Space</kbd> | Pause or resume |
| <kbd>Delete</kbd> | Remove |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy magnet links |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

## Where things live

| Path | What |
| --- | --- |
| `~/.config/torrents/settings.toml` | Settings |
| `~/.config/torrents/categories.toml` | Categories and tags |
| `~/.config/torrents/themes/` | Your own themes |
| `~/.local/share/torrents/` | The session: resume data, DHT state, search plugins |

## License

MIT. libtorrent is BSD-licensed.
