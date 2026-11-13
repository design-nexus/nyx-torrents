//! Torrents — a BitTorrent client for Omarchy, built on libtorrent.

mod actions;
mod add_dialog;
mod cmd;
mod control;
mod engine;
mod fmt;
mod graph;
mod live;
mod model;
mod paths;
mod prefs;
mod search;
mod sections;
mod settings_dialog;
mod store;
mod table;
mod theme;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "io.github.design_nexus.nyx.Torrents";

const USAGE: &str = "Usage: nyx-torrents [--section ID] [--quit] [FILE.torrent | MAGNET | URL]...\n\
\n\
  --section ID   open (or switch the open window) to a page: home, search, all, downloading,\n\
                 seeding, completed, paused, active, inactive, errored, cat:NAME, tag:NAME,\n\
                 downloads, connection, speed, bittorrent, appearance\n\
  --quit         stop the running instance (after saving its state)\n\
\n\
Torrent files, magnet links and links to .torrent files are added to the running window.\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-torrents", name: "Torrents" });
    nyx_core::migrate::from(&["torrents"]);
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }
    glib::set_application_name("Torrents");

    let flags = gio::ApplicationFlags::HANDLES_COMMAND_LINE;
    // Developer aid: a different id gives a separate instance (a test copy on a virtual
    // monitor), so it doesn't hand its arguments to the copy that's open.
    let id = std::env::var("NYX_TORRENTS_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = gtk::Application::builder().application_id(id).flags(flags).build();
    // Nyx Assistant lists, adds and pauses torrents through this (primary instance only).
    app.connect_startup(|app| nyx_core::control::serve(app, control::tools(), control::handle));
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        if argv.iter().any(|a| a == "--quit") {
            app.quit();
            return glib::ExitCode::SUCCESS;
        }
        let mut section = None;
        let mut items = Vec::new();
        let mut i = 1;
        while i < argv.len() {
            if argv[i] == "--section" {
                section = argv.get(i + 1).cloned();
                i += 2;
                continue;
            }
            items.push(argv[i].clone());
            i += 1;
        }
        window::present(app, section.as_deref());
        // Paths are relative to where `torrents` was run, not to the running instance.
        let cwd = cl.cwd();
        for item in items {
            let path = std::path::Path::new(&item);
            if !item.contains("://")
                && !item.starts_with("magnet:")
                && path.is_relative()
                && let Some(cwd) = &cwd
            {
                actions::add_link(&cwd.join(path).to_string_lossy());
                continue;
            }
            actions::add_link(&item);
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| {
        prefs::flush();
        live::shutdown();
    });
    app.run()
}
