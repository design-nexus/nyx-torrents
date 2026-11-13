//! Search: query every enabled site (a qBittorrent search plugin) at once and list
//! what they find. The sites sit under the query as chips that switch them on and
//! off; Add (or a double-click) adds a result. Plugins are managed in a dialog.

use crate::search::{self, Engine, Hit, Message};
use crate::table::{Column, Lead, Table};
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
    hint: gtk::Label,
    button: gtk::Button,
    running: bool,
    banner: gtk::Box,
    sites: gtk::FlowBox,
    sites_label: gtk::Label,
    category: gtk::DropDown,
    dirty: bool,
    /// The site list has been read (it's read off the UI thread).
    loaded: bool,
    /// A search asked for before the site list was read.
    pending: bool,
}

thread_local! {
    static STATE: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
    /// The search field, for Ctrl+F.
    static QUERY: RefCell<Option<gtk::SearchEntry>> = const { RefCell::new(None) };
}

fn state() -> Option<Rc<RefCell<State>>> {
    STATE.with(|s| s.borrow().clone())
}

/// How long ago a result was published: "today", "2 days ago", "3 months ago".
fn ago(ts: i64) -> String {
    if ts <= 0 {
        return "–".into();
    }
    let now = glib::real_time() / 1_000_000;
    let days = (now - ts).max(0) / 86_400;
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => plural(days, "day"),
        14..=59 => plural(days / 7, "week"),
        60..=364 => plural(days / 30, "month"),
        _ => plural(days / 365, "year"),
    }
}

/// The dot beside a result's seeds: how well seeded it is.
fn seed_class(h: &Hit) -> &'static str {
    match h.seeds {
        501.. => "seed-hi",
        151..=500 => "seed-mid",
        _ => "seed-low",
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// 1840 as "1,840".
fn num(v: i64) -> String {
    if v < 0 {
        return "–".into();
    }
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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
        window::toast("Turn on at least one search site first.");
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
        st.status.set_text(&format!("Searching {}…", plural(engines.len(), "site")));
        st.hint.set_visible(false);
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
                text.push_str(&format!(" · {} failed", plural(failed.len(), "site")));
                st.status.set_tooltip_text(Some(&failed.join("\n")));
            } else {
                st.status.set_tooltip_text(None);
            }
            st.status.set_text(&text);
            update_hint(&st);
            st.table.set_empty_text("Nothing found. Try other words, or turn on more sites.");
        },
    );
}

fn reload_engines() {
    cmd::background(search::load_engines, |engines| {
        let Some(st_rc) = state() else { return };
        let pending = {
            let mut st = st_rc.borrow_mut();
            st.engines = engines;
            st.loaded = true;
            std::mem::take(&mut st.pending)
        };
        show_sites(&st_rc.borrow());
        if pending {
            run_entry();
        }
    });
}

/// "Most seeds first" while the results are in that order.
fn update_hint(st: &State) {
    let by_seeds = st
        .table
        .view
        .sorter()
        .and_downcast::<gtk::ColumnViewSorter>()
        .and_then(|s| Some((s.primary_sort_column()?.title()?.to_string(), s.primary_sort_order())))
        .is_some_and(|(title, order)| title == "Seeds" && order == gtk::SortType::Descending);
    st.hint.set_visible(by_seeds && !st.hits.is_empty() && !st.running);
}

/// The sites as chips: on ones carry a check; a click switches one on or off.
fn show_sites(st: &State) {
    let none = st.engines.is_empty();
    st.banner.set_visible(none || !search::python());
    while let Some(c) = st.sites.first_child() {
        st.sites.remove(&c);
    }
    for e in &st.engines {
        let on = search::enabled(e);
        let chip = gtk::Button::new();
        chip.add_css_class("chip");
        chip.add_css_class("site-chip");
        let content = widgets::hbox(5);
        if on {
            chip.add_css_class("selected");
            content.append(&gtk::Image::from_icon_name("object-select-symbolic"));
        }
        content.append(&gtk::Label::new(Some(&e.name)));
        chip.set_child(Some(&content));
        if !e.error.is_empty() {
            chip.set_sensitive(false);
            chip.set_tooltip_text(Some(&format!("Broken: {}", e.error)));
        } else {
            chip.set_tooltip_text(Some(&format!("{} ({})", if on { "Searched" } else { "Not searched" }, e.url)));
        }
        let id = e.id.clone();
        chip.connect_clicked(move |_| {
            let id = id.clone();
            prefs::update(move |p| {
                if !p.disabled_engines.contains(&id) {
                    p.disabled_engines.push(id);
                } else {
                    p.disabled_engines.retain(|x| *x != id);
                }
            });
            if let Some(st) = state() {
                show_sites(&st.borrow());
            }
        });
        st.sites.append(&chip);
    }
    let mut child = st.sites.first_child();
    while let Some(c) = child {
        c.set_focusable(false);
        child = c.next_sibling();
    }
    let enabled = st.engines.iter().filter(|e| search::enabled(e)).count();
    st.sites_label.set_text(&if none {
        "No search sites yet.".to_string()
    } else {
        format!("Searching {enabled} of {}:", plural(st.engines.len(), "site"))
    });
    if !st.running && st.hits.is_empty() {
        st.status.set_text(if none { "Add a search site to start" } else { "" });
    }
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
    let (dialog, card) = widgets::dialog("Search sites", 620);
    dialog.set_default_height(620);
    let note = widgets::label(
        &format!("Each site is a qBittorrent search plugin. They live in ~/.local/share/torrents/search/engines. {WARNING}"),
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
        list.append(&widgets::label("No sites yet.", "empty-state"));
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
            "No search sites yet. Import the plugins qBittorrent has, or install one from a file or link."
        } else {
            "Search sites need <b>python3</b>, which isn't installed. Install it with <tt>sudo pacman -S python</tt>."
        },
        !search::python(),
    );
    if search::python() {
        let b = gtk::Button::with_label("Import from qBittorrent");
        b.set_valign(gtk::Align::Center);
        b.set_sensitive(paths::qbittorrent_engines().is_dir());
        b.connect_clicked(|_| import_qbittorrent());
        banner.append(&b);
        let b = gtk::Button::with_label("Manage sites…");
        b.set_valign(gtk::Align::Center);
        b.connect_clicked(|_| plugins_dialog());
        banner.append(&b);
    }
    banner.set_visible(false);
    banner.add_css_class("search-banner");
    page.body.append(&banner);

    let query = widgets::hbox(8);
    query.add_css_class("search-query");
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Search the sites for torrents"));
    entry.add_css_class("big-search");
    entry.set_hexpand(true);
    entry.set_width_chars(8);
    query.append(&entry);
    QUERY.with(|q| *q.borrow_mut() = Some(entry.clone()));
    let cats: Vec<&str> = search::CATEGORIES.iter().map(|c| c.1).collect();
    let category = gtk::DropDown::from_strings(&cats);
    category.set_valign(gtk::Align::Center);
    let current = prefs::get().search_category;
    category.set_selected(search::CATEGORIES.iter().position(|c| c.0 == current).unwrap_or(0) as u32);
    category.connect_selected_notify(|d| {
        let id = search::CATEGORIES.get(d.selected() as usize).map(|c| c.0).unwrap_or("all");
        prefs::update(|p| p.search_category = id.to_string());
    });
    query.append(&category);
    let go = gtk::Button::with_label("Search");
    go.add_css_class("suggested-action");
    go.set_valign(gtk::Align::Center);
    query.append(&go);
    page.body.append(&query);

    let sites_row = widgets::hbox(8);
    sites_row.add_css_class("search-sites");
    let puzzle = gtk::Image::from_icon_name("application-x-addon-symbolic");
    puzzle.set_valign(gtk::Align::Center);
    sites_row.append(&puzzle);
    let sites_label = widgets::label("", "search-sites-label");
    sites_label.set_valign(gtk::Align::Center);
    sites_row.append(&sites_label);
    let sites = gtk::FlowBox::new();
    sites.set_selection_mode(gtk::SelectionMode::None);
    sites.set_hexpand(true);
    sites.set_valign(gtk::Align::Center);
    sites.set_min_children_per_line(1);
    sites.set_max_children_per_line(24);
    sites.set_row_spacing(6);
    sites.set_column_spacing(6);
    sites_row.append(&sites);
    let manage = gtk::Button::with_label("Manage sites…");
    manage.add_css_class("flat");
    manage.add_css_class("link-button");
    manage.set_valign(gtk::Align::Center);
    manage.connect_clicked(|_| plugins_dialog());
    sites_row.append(&manage);
    page.body.append(&sites_row);

    let head = widgets::hbox(10);
    head.add_css_class("results-head");
    let status = widgets::label("", "results-count");
    head.append(&status);
    let hint = widgets::label("Most seeds first", "results-hint");
    hint.set_visible(false);
    hint.set_hexpand(true);
    head.append(&hint);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    head.append(&spacer);
    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("Filter results"));
    filter.add_css_class("results-filter");
    filter.set_width_chars(16);
    head.append(&filter);
    page.body.append(&head);

    let on_add: crate::table::OnClick = Rc::new(|key: &str| {
        let hit = state().and_then(|st| st.borrow().hits.iter().find(|h| format!("{}|{}", h.engine_id, h.link) == key).cloned());
        if let Some(h) = hit {
            add_hit(h);
        }
    });
    let table = Rc::new(Table::new(
        vec![
            Column::text("Name", 0, |h: &Hit| h.name.clone())
                .sorted(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .lead(Lead::Tile(|h: &Hit| h.name.clone())),
            Column::num(
                "Size",
                90,
                |h| if h.size < 0 { "–".into() } else { fmt::bytes(h.size as f64) },
                |a, b| a.size.cmp(&b.size),
            ),
            Column::num("Seeds", 84, |h: &Hit| num(h.seeds), |a, b| a.seeds.cmp(&b.seeds)).lead(Lead::Dot(seed_class)),
            Column::num("Leechers", 84, |h| num(h.leech), |a, b| a.leech.cmp(&b.leech)),
            Column::text("Site", 120, |h: &Hit| h.engine.clone()).sorted(|a, b| a.engine.cmp(&b.engine)),
            Column::text("Published", 116, |h: &Hit| ago(h.pub_date)).sorted(|a, b| a.pub_date.cmp(&b.pub_date)),
            Column::button(56, "folder-download-symbolic", "Add", on_add),
        ],
        "Type what you're looking for and press Enter.",
    ));
    table.view.add_css_class("results-table");
    // Names stay readable: at 800 px only Size and Seeds keep them company.
    table.set_room(440);
    // Most seeds first.
    if let Some(seeds) = table.view.columns().item(2).and_downcast::<gtk::ColumnViewColumn>() {
        table.view.sort_by_column(Some(&seeds), gtk::SortType::Descending);
    }
    if let Some(sorter) = table.view.sorter() {
        // Also fires while the table is refilled under a borrow of the state.
        sorter.connect_changed(|_, _| {
            glib::idle_add_local_once(|| {
                if let Some(st) = state()
                    && let Ok(st) = st.try_borrow()
                {
                    update_hint(&st);
                }
            });
        });
    }
    table.on_activate(add_hit);
    table.on_secondary(hit_menu);
    table.root.set_vexpand(true);
    page.body.append(&table.root);

    let st = State {
        engines: Vec::new(),
        hits: Vec::new(),
        filter: String::new(),
        table,
        status,
        hint,
        button: go.clone(),
        running: false,
        banner,
        sites,
        sites_label,
        category: category.clone(),
        dirty: false,
        loaded: false,
        pending: false,
    };
    STATE.with(|s| *s.borrow_mut() = Some(Rc::new(RefCell::new(st))));

    let r2 = run_entry;
    go.connect_clicked(move |_| r2());
    entry.connect_activate(move |_| run_entry());
    filter.connect_search_changed(|e| {
        if let Some(st) = state() {
            st.borrow_mut().filter = e.text().to_string();
            show_hits(&st.borrow());
        }
    });
    reload_engines();
}

/// Search for what's in the field, or stop the search that's running.
fn run_entry() {
    let Some(st) = state() else { return };
    if st.borrow().running {
        search::stop();
        return;
    }
    if !st.borrow().loaded {
        st.borrow_mut().pending = true;
        return;
    }
    let cat = search::CATEGORIES.get(st.borrow().category.selected() as usize).map(|c| c.0).unwrap_or("all");
    start(&query(), cat);
}

/// What's in the search field.
pub fn query() -> String {
    QUERY.with(|q| q.borrow().as_ref().map(|e| e.text().trim().to_string()).unwrap_or_default())
}

/// Put `text` in the search field and search for it (a saved search).
pub fn run_query(text: &str) {
    QUERY.with(|q| {
        if let Some(e) = q.borrow().as_ref() {
            e.set_text(text);
        }
    });
    if state().is_some_and(|s| !s.borrow().running) {
        run_entry();
    }
}

/// Put the cursor in the search field.
pub fn focus_query() {
    QUERY.with(|q| {
        if let Some(e) = q.borrow().as_ref() {
            e.grab_focus();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_numbers_and_dates_plainly() {
        assert_eq!(num(1840), "1,840");
        assert_eq!(num(1_234_567), "1,234,567");
        assert_eq!(num(7), "7");
        assert_eq!(num(-1), "–");
        let now = glib::real_time() / 1_000_000;
        assert_eq!(ago(now - 2 * 86_400), "2 days ago");
        assert_eq!(ago(now - 42 * 86_400), "6 weeks ago");
    }
}
