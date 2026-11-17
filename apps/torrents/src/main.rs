//! Torrents — a BitTorrent client for NyxOS and Omarchy, built on libtorrent.

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
    nyx_core::control::describe_if_asked(&control::description(
        &std::env::var("NYX_TORRENTS_APP_ID").unwrap_or_else(|_| APP_ID.to_string()),
    ));
    nyx_core::migrate::from(&["torrents"]);
    let mut args: Vec<String> = std::env::args().collect();
    // Started by D-Bus for an action: GLib leaves this flag to apps that handle their own
    // command line.
    let service = args.iter().any(|a| a == "--gapplication-service");
    args.retain(|a| a != "--gapplication-service");
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }
    glib::set_application_name("Torrents");

    let flags = if service {
        gio::ApplicationFlags::HANDLES_COMMAND_LINE | gio::ApplicationFlags::IS_SERVICE
    } else {
        gio::ApplicationFlags::HANDLES_COMMAND_LINE
    };
    // Developer aid: a different id gives a separate instance (a test copy on a virtual
    // monitor), so it doesn't hand its arguments to the copy that's open.
    let id = std::env::var("NYX_TORRENTS_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    let app = gtk::Application::builder().application_id(id).flags(flags).build();
    // Nyx Assistant lists, adds and pauses torrents through this (primary instance only).
    // The nyx command, Glimpse and Nyx Assistant reach it through this (primary instance
    // only); a call to a closed app opens its window, which holds what the actions use.
    let described = control::description(&app.application_id().map(|s| s.to_string()).unwrap_or_default());
    app.connect_startup(move |app| {
        let handle_app = app.clone();
        nyx_core::control::serve_async(app, described.clone(), move |name, args, reply| {
            if handle_app.active_window().is_none() {
                window::present(&handle_app, None);
            }
            // Just opened for this call: the engine reports its torrents a moment later.
            let (name, tries) = (name.to_string(), std::cell::Cell::new(0));
            let pending = std::cell::RefCell::new(Some((args, reply)));
            glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                tries.set(tries.get() + 1);
                if control::starting() && tries.get() < 100 {
                    return glib::ControlFlow::Continue;
                }
                if let Some((args, reply)) = pending.borrow_mut().take() {
                    reply(control::handle(&name, args));
                }
                glib::ControlFlow::Break
            });
        })
    });
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
    app.run_with_args(&args)
}
