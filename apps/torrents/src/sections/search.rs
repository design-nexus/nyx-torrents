//! Search: query every enabled plugin at once and list what they find. Double-click
//! a result to add it. Plugins are managed in a dialog from the toolbar.

use crate::search::{self, Engine, Hit, Message};
use crate::table::{Column, Table};
use crate::widgets::{self, Page};
use crate::{actions, cmd, fmt, paths, prefs, window};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

struct State {
    engines: Vec<Engine>,
    hits: Vec<Hit>,
    filter: String,
    table: Rc<Table<Hit>>,
    status: gtk::Label,
    button: gtk::Button,
    running: bool,
    banner: gtk::Box,
    dirty: bool,
}

thread_local! {
    static STATE: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
    /// The search field, for Ctrl+F.
    static QUERY: RefCell<Option<gtk::SearchEntry>> = const { RefCell::new(None) };
}

fn state() -> Option<Rc<RefCell<State>>> {
    STATE.with(|s| s.borrow().clone())
}

fn date(ts: i64) -> String {
    if ts <= 0 {
        return String::new();
    }
    glib::DateTime::from_unix_local(ts).ok().and_then(|d| d.format("%Y-%m-%d").ok()).map(|s| s.to_string()).unwrap_or_default()
}

fn num(v: i64) -> String {
    if v < 0 { "–".into() } else { v.to_string() }
}

fn show_hits(st: &State) {
    let q = st.filter.to_lowercase();
    let rows: Vec<(String, Hit)> = st
        .hits
        .iter()
        .filter(|h| q.split_whitespace().all(|t| h.name.to_lowercase().contains(t)))
        .map(|h| (format!("{}|{}", h.engine_id, h.link), h.clone()))
        .collect();
    st.table.set(rows);
}

fn add_hit(hit: Hit) {
    if hit.link.starts_with("magnet:") {
        actions::add_link(&hit.link);
        return;
    }
    window::toast(&format!("Fetching {}…", hit.name));
    search::download(&hit.engine_id, &hit.link, |res| match res {
        Ok(target) if target.starts_with("magnet:") => actions::add_link(&target),
        Ok(path) => {
            let p = std::path::PathBuf::from(&path);
            actions::add_torrent_path(&p);
            if p.starts_with(std::env::temp_dir()) {
                let _ = std::fs::remove_file(&p);
            }
        }
        Err(e) => window::toast(&format!("Couldn't get the torrent: {e}")),
    });
}

fn hit_menu(view: &gtk::ColumnView, hit: Hit, x: f64, y: f64) {
    let group = gio::SimpleActionGroup::new();
    let menu = gio::Menu::new();
    let add = gio::SimpleAction::new("add", None);
    let h = hit.clone();
    add.connect_activate(move |_, _| add_hit(h.clone()));
    group.add_action(&add);
    menu.append(Some("Add"), Some("sr.add"));
    if !hit.desc_link.is_empty() {
        let open = gio::SimpleAction::new("open", None);
        let link = hit.desc_link.clone();
        open.connect_activate(move |_, _| cmd::spawn(&["xdg-open", &link]));
        group.add_action(&open);
        menu.append(Some("Open description page"), Some("sr.open"));
    }
    let copy = gio::SimpleAction::new("copy", None);
    let link = hit.link.clone();
    copy.connect_activate(move |_, _| {
        if let Some(d) = gdk::Display::default() {
            d.clipboard().set_text(&link);
        }
    });
    group.add_action(&copy);
    menu.append(Some("Copy link"), Some("sr.copy"));
    view.insert_action_group("sr", Some(&group));
    let pop = gtk::PopoverMenu::from_model(Some(&menu));
    pop.set_parent(view);
    pop.set_has_arrow(false);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

fn start(query: &str, category: &str) {
    let Some(st_rc) = state() else { return };
    let engines: Vec<Engine> = st_rc.borrow().engines.iter().filter(|e| search::enabled(e)).cloned().collect();
    if engines.is_empty() {
        window::toast("Turn on at least one search plugin first.");
        return;
    }
    if query.trim().is_empty() {
        return;
    }
    {
        let mut st = st_rc.borrow_mut();
        st.hits.clear();
        st.running = true;
        st.button.set_label("Stop");
        st.button.remove_css_class("suggested-action");
        st.status.set_text(&format!("Searching {} plugins…", engines.len()));
        st.table.set_empty_text("Searching…");
        show_hits(&st);
    }
    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let f2 = failures.clone();
    let st2 = st_rc.clone();
    // Results stream in; redraw the table at most a few times a second.
    let flush = glib::timeout_add_local(std::time::Duration::from_millis(300), move || {
        let mut st = st2.borrow_mut();
        if st.dirty {
            st.dirty = false;
            show_hits(&st);
        }
        glib::ControlFlow::Continue
    });
    let flush = Rc::new(RefCell::new(Some(flush)));
    let st3 = st_rc.clone();
    search::run(
        query,
        category,
        engines,
        move |m| match m {
            Message::Hit(h) => {
                let mut st = st_rc.borrow_mut();
                if st.hits.len() < 5000 {
                    st.hits.push(h);
                    st.dirty = true;
                    let n = st.hits.len();
                    st.status.set_text(&format!("{n} results so far…"));
                }
            }
            Message::EngineDone { name, error } => {
                if !error.is_empty() && !error.starts_with("Connection error: <urlopen error [Errno") {
                    f2.borrow_mut().push(format!("{name}: {error}"));
                } else if !error.is_empty() {
                    f2.borrow_mut().push(format!("{name}: couldn't connect"));
                }
            }
        },
        move || {
            if let Some(id) = flush.borrow_mut().take() {
                id.remove();
            }
            let mut st = st3.borrow_mut();
            st.running = false;
            st.button.set_label("Search");
            st.button.add_css_class("suggested-action");
            show_hits(&st);
            let n = st.hits.len();
            let mut text = format!("{n} result{}", if n == 1 { "" } else { "s" });
            let failed = failures.borrow();
            if !failed.is_empty() {
                text.push_str(&format!(" · {} plugin{} failed", failed.len(), if failed.len() == 1 { "" } else { "s" }));
                st.status.set_tooltip_text(Some(&failed.join("\n")));
            } else {
                st.status.set_tooltip_text(None);
            }
            st.status.set_text(&text);
            st.table.set_empty_text("Nothing found. Try other words, or turn on more plugins.");
        },
    );
}

fn reload_engines() {
    cmd::background(search::load_engines, |engines| {
        let Some(st_rc) = state() else { return };
        let mut st = st_rc.borrow_mut();
        st.engines = engines;
        let none = st.engines.is_empty();
        st.banner.set_visible(none || !search::python());
        let enabled = st.engines.iter().filter(|e| search::enabled(e)).count();
        if !st.running {
            st.status.set_text(&if none {
                "No plugins installed".to_string()
            } else {
                format!("{enabled} of {} plugins on", st.engines.len())
            });
        }
    });
}

fn import_qbittorrent() {
    match search::import_qbittorrent() {
        Ok(n) => {
            window::toast(&format!("Imported {n} plugins from qBittorrent."));
            reload_engines();
        }
        Err(e) => window::toast(&format!("Couldn't import from qBittorrent: {e}")),
    }
}

fn install_from_file() {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Search plugins"));
    filter.add_pattern("*.py");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Install a search plugin you trust").modal(true).filters(&filters).build();
    dialog.open(window::window().as_ref(), gio::Cancellable::NONE, |res| {
        let Ok(f) = res else { return };
        let Some(p) = f.path() else { return };
        match search::install_file(&p) {
            Ok(id) => {
                window::toast(&format!("Installed {id}."));
                reload_engines();
            }
            Err(e) => window::toast(&e),
        }
    });
}

/// Said wherever a plugin can be installed: a plugin is a program, not a list of sites.
const WARNING: &str = "A plugin is a program: every search runs it with access to all your files. Install only plugins you trust.";

fn install_from_url() {
    let (dialog, card) = widgets::dialog("Install from a link", 520);
    card.append(&widgets::label("A secure (https) link to a plugin's .py file, like the ones on qBittorrent's plugin wiki.", "dim"));
    card.append(&widgets::label(WARNING, "dim"));
    let entry = gtk::Entry::new();
    entry.add_css_class("mono");
    entry.set_placeholder_text(Some("https://…/plugin.py"));
    card.append(&entry);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Install");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        let url = entry.text().trim().to_string();
        d.close();
        if !url.starts_with("https://") {
            window::toast("Use a secure link (https://).");
            return;
        }
        // A folder of our own (0700), not a name anyone could predict in the shared /tmp.
        let name = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
        let name = if name.ends_with(".py") && !name.starts_with('.') { name } else { "plugin.py".to_string() };
        let dir = gtk::glib::user_runtime_dir().join(format!("nyx-torrents-{}", std::process::id()));
        let made = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)));
        if let Err(e) = made {
            window::toast(&format!("Couldn't install it: {e}"));
            return;
        }
        let tmp = dir.join(name);
        let tmp2 = tmp.clone();
        let args = ["curl", "-fsSL", "--proto", "=https", "--proto-redir", "=https", "--max-time", "30", "-o", &tmp.to_string_lossy(), &url];
        cmd::run_async(&args, move |res| {
            match res.map_err(|e| e.to_string()).and_then(|_| search::install_file(&tmp2)) {
                Ok(id) => {
                    window::toast(&format!("Installed {id}."));
                    reload_engines();
                }
                Err(e) => window::toast(&format!("Couldn't install it: {e}")),
            }
            let _ = std::fs::remove_file(&tmp2);
        });
    });
    dialog.present();
}

fn plugins_dialog() {
    let Some(st_rc) = state() else { return };
    let engines = st_rc.borrow().engines.clone();
    let (dialog, card) = widgets::dialog("Search plugins", 620);
    dialog.set_default_height(620);
    let note = widgets::label(
        &format!("Plugins use qBittorrent's format. They live in ~/.local/share/torrents/search/engines. {WARNING}"),
        "dim",
    );
    note.set_wrap(true);
    card.append(&note);
    let list = widgets::vbox(6);
    for e in &engines {
        let desc = if !e.error.is_empty() {
            format!("<span foreground=\"red\">Broken: {}</span>", glib::markup_escape_text(&e.error))
        } else {
            let cats: Vec<&str> = e.categories.iter().map(String::as_str).filter(|c| *c != "all").collect();
            format!(
                "<tt>{}</tt>{}{}",
                glib::markup_escape_text(&e.url),
                if e.version.is_empty() { String::new() } else { format!(" · v{}", e.version) },
                if cats.is_empty() { String::new() } else { format!(" · {}", cats.join(", ")) }
            )
        };
        let controls = widgets::hbox(8);
        let sw = gtk::Switch::new();
        sw.set_active(search::enabled(e));
        sw.set_sensitive(e.error.is_empty());
        sw.set_valign(gtk::Align::Center);
        let id = e.id.clone();
        sw.connect_active_notify(move |s| {
            let (on, id) = (s.is_active(), id.clone());
            prefs::update(move |p| {
                p.disabled_engines.retain(|x| *x != id);
                if !on {
                    p.disabled_engines.push(id);
                }
            });
            reload_engines();
        });
        let (id, name, list2, row_box) = (e.id.clone(), e.name.clone(), list.clone(), controls.clone());
        let remove = widgets::two_click("Remove", "Click again", move || {
            search::remove(&id);
            // The row is the card holding these controls.
            if let Some(row) = row_box.parent() {
                list2.remove(&row);
            }
            window::toast(&format!("Removed {name}."));
            reload_engines();
        });
        controls.append(&remove);
        controls.append(&sw);
        let r = widgets::row(&e.name, &desc, Some(controls.upcast_ref()));
        list.append(&r);
    }
    if engines.is_empty() {
        list.append(&widgets::label("No plugins yet.", "empty-state"));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&list)
        .vexpand(true)
        .build();
    card.append(&scroll);
    let buttons = widgets::hbox(8);
    let import = gtk::Button::with_label("Import from qBittorrent");
    import.set_sensitive(paths::qbittorrent_engines().is_dir());
    let d = dialog.clone();
    import.connect_clicked(move |_| {
        d.close();
        import_qbittorrent();
    });
    let from_file = gtk::Button::with_label("Install from file…");
    let d = dialog.clone();
    from_file.connect_clicked(move |_| {
        d.close();
        install_from_file();
    });
    let from_url = gtk::Button::with_label("From a link…");
    let d = dialog.clone();
    from_url.connect_clicked(move |_| {
        d.close();
        install_from_url();
    });
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let close = gtk::Button::with_label("Done");
    close.add_css_class("suggested-action");
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    buttons.append(&import);
    buttons.append(&from_file);
    buttons.append(&from_url);
    buttons.append(&spacer);
    buttons.append(&close);
    card.append(&buttons);
    dialog.present();
}

pub fn build(page: &Page) {
    search::ensure_installed();

    let banner = widgets::banner(
        if search::python() {
            "No search plugins yet. Import the ones qBittorrent has, or install one from a file or link."
        } else {
            "Search plugins need <b>python3</b>, which isn't installed. Install it with <tt>sudo pacman -S python</tt>."
        },
        !search::python(),
    );
    if search::python() {
        let b = gtk::Button::with_label("Import from qBittorrent");
        b.set_valign(gtk::Align::Center);
        b.set_sensitive(paths::qbittorrent_engines().is_dir());
        b.connect_clicked(|_| import_qbittorrent());
        banner.append(&b);
        let b = gtk::Button::with_label("Plugins…");
        b.set_valign(gtk::Align::Center);
        b.connect_clicked(|_| plugins_dialog());
        banner.append(&b);
    }
    banner.set_visible(false);
    page.body.append(&banner);

    let toolbar = widgets::hbox(8);
    toolbar.add_css_class("toolbar");
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search for torrents"));
    entry.set_hexpand(true);
    entry.set_width_chars(8);
    toolbar.append(&entry);
    QUERY.with(|q| *q.borrow_mut() = Some(entry.clone()));
    let cats: Vec<&str> = search::CATEGORIES.iter().map(|c| c.1).collect();
    let category = gtk::DropDown::from_strings(&cats);
    let current = prefs::get().search_category;
    category.set_selected(search::CATEGORIES.iter().position(|c| c.0 == current).unwrap_or(0) as u32);
    category.connect_selected_notify(|d| {
        let id = search::CATEGORIES.get(d.selected() as usize).map(|c| c.0).unwrap_or("all");
        prefs::update(|p| p.search_category = id.to_string());
    });
    toolbar.append(&category);
    let go = gtk::Button::with_label("Search");
    go.add_css_class("suggested-action");
    toolbar.append(&go);
    let plugins = gtk::Button::from_icon_name("application-x-addon-symbolic");
    plugins.set_tooltip_text(Some("Search plugins"));
    plugins.connect_clicked(|_| plugins_dialog());
    toolbar.append(&plugins);
    page.body.append(&toolbar);

    let sub = widgets::hbox(8);
    sub.add_css_class("search-sub");
    let status = widgets::label("", "dim");
    status.set_hexpand(true);
    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("Filter results"));
    filter.set_width_chars(16);
    sub.append(&status);
    sub.append(&filter);
    page.body.append(&sub);

    let table = Rc::new(Table::new(
        vec![
            Column::text("Name", 0, |h: &Hit| h.name.clone()).sorted(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            Column::num(
                "Size",
                90,
                |h| if h.size < 0 { "–".into() } else { fmt::bytes(h.size as f64) },
                |a, b| a.size.cmp(&b.size),
            ),
            Column::num("Seeds", 70, |h| num(h.seeds), |a, b| a.seeds.cmp(&b.seeds)),
            Column::num("Leechers", 80, |h| num(h.leech), |a, b| a.leech.cmp(&b.leech)),
            Column::text("Plugin", 120, |h: &Hit| h.engine.clone()).sorted(|a, b| a.engine.cmp(&b.engine)),
            Column::num("Published", 110, |h| date(h.pub_date), |a, b| a.pub_date.cmp(&b.pub_date)),
        ],
        "Type what you're looking for and press Enter.",
    ));
    // Most seeds first.
    if let Some(seeds) = table.view.columns().item(2).and_downcast::<gtk::ColumnViewColumn>() {
        table.view.sort_by_column(Some(&seeds), gtk::SortType::Descending);
    }
    table.on_activate(add_hit);
    table.on_secondary(hit_menu);
    let card = widgets::vbox(0);
    card.add_css_class("table-card");
    card.set_overflow(gtk::Overflow::Hidden);
    card.append(&table.root);
    card.set_vexpand(true);
    page.body.append(&card);

    let st = State {
        engines: Vec::new(),
        hits: Vec::new(),
        filter: String::new(),
        table,
        status,
        button: go.clone(),
        running: false,
        banner,
        dirty: false,
    };
    STATE.with(|s| *s.borrow_mut() = Some(Rc::new(RefCell::new(st))));

    let run = {
        let (entry, category) = (entry.clone(), category.clone());
        move || {
            if state().is_some_and(|s| s.borrow().running) {
                search::stop();
                return;
            }
            let cat = search::CATEGORIES.get(category.selected() as usize).map(|c| c.0).unwrap_or("all");
            start(&entry.text(), cat);
        }
    };
    let r2 = run.clone();
    go.connect_clicked(move |_| r2());
    entry.connect_activate(move |_| run());
    filter.connect_search_changed(|e| {
        if let Some(st) = state() {
            st.borrow_mut().filter = e.text().to_string();
            show_hits(&st.borrow());
        }
    });
    reload_engines();
}

/// Put the cursor in the search field.
pub fn focus_query() {
    QUERY.with(|q| {
        if let Some(e) = q.borrow().as_ref() {
            e.grab_focus();
        }
    });
}
