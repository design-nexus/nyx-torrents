//! The main window: the header (the app and the sidebar toggle; a search-the-sites
//! field in the middle; the overall speeds, the slow-mode chip, Add and Settings at
//! the end), the navigation sidebar (Home and Search, the transfer filters with live
//! counts, saved searches, categories and tags) and a stack of pages built the first
//! time they're shown. Every transfer filter shows the same Transfers page. The
//! settings pages open in the settings dialog instead (see `settings_dialog`).

use crate::model::{Filter, STATUS_FILTERS};
use crate::sections::{self, Section, home, transfers};
use crate::widgets;
use crate::{actions, fmt, live, prefs, settings_dialog, store, theme};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use nyx_ui::chrome;
use nyx_ui::sidebar::{self, ACCENT_SECTION};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The transfer filters the sidebar lists (the rest stay reachable with `--section`).
const SIDEBAR_FILTERS: &[&str] = &["downloading", "seeding", "completed", "paused", "errored"];

/// Category dots take these swatches in turn.
const CATEGORY_SWATCHES: &[&str] = &["blue", "coral", "teal", "pink", "amber", "green", "violet", "red"];

/// Where a saved search's nav id starts.
const SAVED: &str = "saved:";

struct Ui {
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    /// Nav buttons by id: page ids, filter ids ("downloading", "cat:Movies"…) and saved searches.
    nav_items: HashMap<String, gtk::Button>,
    nav_counts: HashMap<String, gtk::Label>,
    /// Rows shown only while they have torrents (Uncategorized, Untagged).
    only_when_used: HashMap<String, gtk::Button>,
    lists_box: gtk::Box,
    pages: HashMap<&'static str, gtk::ScrolledWindow>,
    sections: Vec<Section>,
    current: String,
    overlay: gtk::Overlay,
    nav: gtk::Box,
    /// Hidden in a narrow window: the speeds, the slow chip's word, Add's key hint.
    wide_only: Vec<gtk::Widget>,
    /// The header's search-the-sites field (hidden on Search, which has its own).
    find: gtk::Button,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPACT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if let Some(ui) = ui() {
        let window = ui.borrow().window.clone();
        if let Some(s) = section {
            navigate(s);
        }
        window.present();
        return;
    }
    theme::install();
    install_icons();
    live::start();
    actions::install(app);
    build(app);
    let start = section.map(String::from).unwrap_or_else(|| nyx_theme::start_page("home", &prefs::get().last_section));
    navigate(&start);
    if let Some(ui) = ui() {
        ui.borrow().window.present();
    }
}

/// Our own symbolic icons (the icon theme may lack a turtle or a magnet).
fn install_icons() {
    const ICONS: &[(&str, &str)] = &[
        ("torrents-turtle-symbolic.svg", include_str!("../data/icons/torrents-turtle-symbolic.svg")),
        ("torrents-magnet-symbolic.svg", include_str!("../data/icons/torrents-magnet-symbolic.svg")),
    ];
    let dir = crate::paths::cache_dir().join("icons");
    for (name, svg) in ICONS {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = crate::cmd::atomic_write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}

/// What a nav row starts with: an icon, or a dot in a swatch (categories).
enum Lead<'a> {
    Icon(&'a str),
    Dot(&'a str),
}

fn nav_button(id: &str, title: &str, lead: Lead, tooltip: &str, count: bool) -> (gtk::Button, Option<gtk::Label>) {
    let button = gtk::Button::new();
    button.add_css_class("nav-item");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    match lead {
        Lead::Icon(icon) => {
            let image = gtk::Image::from_icon_name(icon);
            image.add_css_class("nav-icon");
            content.append(&image);
        }
        Lead::Dot(swatch) => {
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.add_css_class("nav-dot");
            dot.add_css_class(&format!("section-swatch-{swatch}"));
            dot.set_valign(gtk::Align::Center);
            content.append(&dot);
        }
    }
    let l = widgets::label(title, "nav-label");
    l.add_css_class("compact-hide");
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&l);
    let readout = count.then(|| {
        let r = widgets::label("", "nav-readout");
        r.add_css_class("tnum");
        r.add_css_class("compact-hide");
        r.set_visible(false);
        content.append(&r);
        r
    });
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(title)]);
    let id = id.to_string();
    button.connect_clicked(move |_| navigate(&id));
    (button, readout)
}

/// A group's heading in its section's colour, after the short bar the icon rail
/// shows in its place. `section`: [`ACCENT_SECTION`] or a name in the theme's
/// [sections.torrents].
fn group_heading(list: &gtk::Box, title: &str, section: &str, add: Option<(&str, fn())>) {
    let bar = gtk::Separator::new(gtk::Orientation::Horizontal);
    bar.add_css_class("nav-divider");
    bar.add_css_class("compact-show");
    bar.add_css_class(&format!("section-{section}"));
    bar.set_visible(COMPACT.with(|c| c.get()));
    list.append(&bar);
    let row = match add {
        Some((tip, f)) => sidebar::group_label_with_add(title, false, tip, f),
        None => sidebar::group_label(title, false),
    };
    sidebar::set_section(&row, section);
    row.set_visible(!COMPACT.with(|c| c.get()));
    list.append(&row);
}

fn build(app: &gtk::Application) {
    let window =
        gtk::ApplicationWindow::builder().application(app).title("Torrents").default_width(1280).default_height(800).build();
    window.add_css_class("torrents-window");
    // No client-side titlebar: Hyprland manages the window.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common { settings: Some(|| settings_dialog::open(None)), shortcuts: Some(show_shortcuts), close: false, palette: None });
    window.set_icon_name(Some("io.github.design_nexus.nyx.Torrents"));

    let sections = sections::all();

    // ----- Sidebar -----
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut nav_items = HashMap::new();
    let mut nav_counts = HashMap::new();

    for (id, title, icon) in [("home", "Home", "go-home-symbolic"), ("search", "Search", "system-search-symbolic")] {
        let (b, _) = nav_button(id, title, Lead::Icon(icon), title, false);
        list.append(&b);
        nav_items.insert(id.to_string(), b);
    }

    group_heading(&list, "Transfers", ACCENT_SECTION, None);
    for id in SIDEBAR_FILTERS {
        let Some((_, title, icon)) = STATUS_FILTERS.iter().find(|f| f.0 == *id) else { continue };
        let (b, count) = nav_button(id, title, Lead::Icon(icon), title, true);
        if let Some(c) = count {
            nav_counts.insert(id.to_string(), c);
        }
        list.append(&b);
        nav_items.insert(id.to_string(), b);
    }

    // Saved searches, categories and tags are filled by `rebuild_lists`.
    let lists_box = widgets::vbox(0);
    list.append(&lists_box);

    let nav_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    nav.append(&nav_scroll);

    // ----- Content -----
    let stack = gtk::Stack::new();
    stack.add_css_class("settings-content");
    stack.set_hexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });

    // ----- Header -----
    let header = chrome::app_header(&window, "torrents-magnet-symbolic", "Torrents");
    let collapse = widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);

    let find = gtk::Button::new();
    find.add_css_class("header-find");
    let find_content = widgets::hbox(8);
    find_content.append(&gtk::Image::from_icon_name("system-search-symbolic"));
    let find_text = widgets::label("Search sites for torrents", "header-find-text");
    find_text.set_hexpand(true);
    find_text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    find_content.append(&find_text);
    let find_key = widgets::label("Ctrl K", "key-hint");
    find_key.set_valign(gtk::Align::Center);
    find_content.append(&find_key);
    find.set_child(Some(&find_content));
    // Its own width, not the header's middle: the label inside expands.
    find.set_hexpand(false);
    find.set_tooltip_text(Some("Search the sites for torrents (Ctrl+K)"));
    find.connect_clicked(|_| open_search());
    header.center.append(&find);

    let mut wide_only: Vec<gtk::Widget> = Vec::new();
    header.end.set_spacing(4);
    let readout = header_readout();
    wide_only.push(readout.clone().upcast());
    header.end.append(&readout);
    let (chip, chip_label) = alt_speed_chip();
    wide_only.push(chip_label.upcast());
    header.end.append(&chip);
    let add = gtk::Button::new();
    add.add_css_class("header-add");
    let add_content = widgets::hbox(6);
    add_content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    add_content.append(&gtk::Label::new(Some("Add")));
    let add_key = widgets::label("Ctrl O", "key-hint");
    add_key.set_valign(gtk::Align::Center);
    wide_only.push(add_key.clone().upcast());
    add_content.append(&add_key);
    add.set_child(Some(&add_content));
    add.set_tooltip_text(Some("Add a .torrent file (Ctrl+O); Ctrl+U adds a magnet link"));
    add.connect_clicked(|_| actions::open_torrent_files());
    header.end.append(&add);
    header.end.append(&chrome::settings_button(|| settings_dialog::open(None)));
    // A tiled half-screen header shows the app's icon without its name.
    fn find_class(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
        if w.has_css_class(class) {
            return Some(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(found) = find_class(&c, class) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    wide_only.extend(find_class(header.root.upcast_ref(), "app-title"));

    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_sidebar(&nav);
    frame.set_page(&stack);
    let overlay = frame.install(&window);

    // ----- Keys -----
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w2 = window.clone();
    let app2 = app.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        if settings_dialog::is_open() {
            return match key {
                gdk::Key::Escape => {
                    settings_dialog::escape();
                    glib::Propagation::Stop
                }
                gdk::Key::f if ctrl => {
                    settings_dialog::focus_search();
                    glib::Propagation::Stop
                }
                gdk::Key::q if ctrl => {
                    app2.quit();
                    glib::Propagation::Stop
                }
                gdk::Key::w if ctrl => {
                    w2.close();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        match key {
            gdk::Key::f if ctrl => {
                find_in_page();
                glib::Propagation::Stop
            }
            gdk::Key::k if ctrl => {
                open_search();
                glib::Propagation::Stop
            }
            gdk::Key::F1 => {
                show_shortcuts();
                glib::Propagation::Stop
            }
            gdk::Key::o if ctrl => {
                actions::open_torrent_files();
                glib::Propagation::Stop
            }
            gdk::Key::u if ctrl => {
                actions::add_magnet_dialog("");
                glib::Propagation::Stop
            }
            gdk::Key::b if ctrl => {
                toggle_sidebar();
                glib::Propagation::Stop
            }
            gdk::Key::q if ctrl => {
                app2.quit();
                glib::Propagation::Stop
            }
            gdk::Key::w if ctrl => {
                w2.close();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(keys);

    // Drop .torrent files or magnet links anywhere on the window.
    let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
    drop.set_types(&[gdk::FileList::static_type(), glib::Type::STRING]);
    drop.connect_drop(|_, value, _, _| {
        if let Ok(files) = value.get::<gdk::FileList>() {
            for f in files.files() {
                if let Some(p) = f.path() {
                    actions::add_torrent_path(&p);
                } else {
                    actions::add_link(&f.uri());
                }
            }
            return true;
        }
        if let Ok(text) = value.get::<String>() {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                actions::add_link(line);
            }
            return true;
        }
        false
    });
    window.add_controller(drop);

    // Closing the window either hides it (torrents keep going) or quits.
    let app3 = app.clone();
    window.connect_close_request(move |w| {
        if prefs::get().keep_running {
            w.set_visible(false);
            glib::Propagation::Stop
        } else {
            app3.quit();
            glib::Propagation::Proceed
        }
    });

    // Narrow windows (a tiled half-screen) get an icon-only sidebar.
    let apply_width = {
        let nav = nav.clone();
        move |w: &gtk::ApplicationWindow| {
            let width = if w.width() > 0 { w.width() } else { w.default_width() };
            let narrow = width > 0 && width < sidebar::NARROW_WIDTH;
            settings_dialog::fit(w);
            if narrow == NARROW.with(|n| n.get()) && nav.has_css_class("sized") {
                return;
            }
            nav.add_css_class("sized");
            set_narrow(narrow);
            apply_compact(narrow || prefs::get().sidebar_collapsed);
        }
    };
    let aw = apply_width.clone();
    window.connect_default_width_notify(move |w| aw(w));
    let aw = apply_width.clone();
    window.connect_realize(move |w| aw(w));
    // Tiled windows are resized by the compositor without touching the default
    // size: an invisible layer over the whole window reports each real size
    // change, and the layout follows on the next frame.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    {
        let (aw, w2) = (apply_width.clone(), window.clone());
        probe.connect_resize(move |_, _, _| {
            let (aw, w2) = (aw.clone(), w2.clone());
            // After this layout pass, when the window's width is the new one.
            glib::idle_add_local_once(move || aw(&w2));
        });
    }
    // A slow fallback, in case a resize slips by.
    let w2 = window.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        apply_width(&w2);
        glib::ControlFlow::Continue
    });

    let ui = Ui {
        app: app.clone(),
        window,
        stack,
        nav_items,
        nav_counts,
        only_when_used: HashMap::new(),
        lists_box,
        pages: HashMap::new(),
        sections,
        current: String::new(),
        overlay,
        nav: nav.clone(),
        wide_only,
        find,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));
    rebuild_lists();
    store::on_lists_changed(rebuild_lists);
    live::on_update(update_counts);
}

thread_local! {
    static ALT_CHIPS: RefCell<Vec<gtk::Button>> = const { RefCell::new(Vec::new()) };
}

/// The turtle chip: alternative speed limits on or off. Also gives back its word.
fn alt_speed_chip() -> (gtk::Button, gtk::Label) {
    let b = gtk::Button::new();
    b.add_css_class("chip");
    b.add_css_class("alt-chip");
    let content = widgets::hbox(6);
    content.append(&gtk::Image::from_icon_name("torrents-turtle-symbolic"));
    let word = widgets::label("Slow", "");
    content.append(&word);
    b.set_child(Some(&content));
    b.connect_clicked(|_| actions::toggle_alt_speed());
    ALT_CHIPS.with(|c| c.borrow_mut().push(b.clone()));
    refresh_alt_speed();
    (b, word)
}

pub fn refresh_alt_speed() {
    let on = live::alt_active();
    let p = prefs::get();
    let tip = format!(
        "Slow mode: alternative speed limits ({} down, {} up){}",
        limit_text(p.alt_dl_limit),
        limit_text(p.alt_ul_limit),
        if p.schedule { ", on a schedule" } else { "" }
    );
    ALT_CHIPS.with(|c| {
        for b in c.borrow().iter() {
            b.set_tooltip_text(Some(&tip));
            if on {
                b.add_css_class("selected");
            } else {
                b.remove_css_class("selected");
            }
        }
    });
}

pub fn limit_text(kib: i32) -> String {
    if kib <= 0 { "unlimited".into() } else { fmt::rate(kib as f64 * 1024.0) }
}

/// Save what's in the search field, or ask for a search to keep.
fn add_saved_search() {
    let save = |q: String| {
        prefs::update(|p| {
            if !p.saved_searches.contains(&q) {
                p.saved_searches.push(q);
            }
        });
        rebuild_lists();
    };
    let q = sections::search::query();
    if !q.is_empty() && !prefs::get().saved_searches.contains(&q) {
        save(q);
        return;
    }
    widgets::ask_text("Save a search", "It stays in the sidebar; click it to search the sites again.", &q, "Save", save);
}

/// Right-click a saved search: search again or remove it.
fn attach_saved_menu(button: &gtk::Button, query: &str) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    let (b, q) = (button.clone(), query.to_string());
    click.connect_pressed(move |_, _, x, y| {
        let group = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        let again = gio::SimpleAction::new("again", None);
        let q2 = q.clone();
        again.connect_activate(move |_, _| navigate(&format!("{SAVED}{q2}")));
        group.add_action(&again);
        menu.append(Some("Search again"), Some("tr.again"));
        let remove = gio::SimpleAction::new("forget", None);
        let q2 = q.clone();
        remove.connect_activate(move |_, _| {
            let q3 = q2.clone();
            prefs::update(move |p| p.saved_searches.retain(|s| *s != q3));
            rebuild_lists();
        });
        group.add_action(&remove);
        menu.append(Some("Remove"), Some("tr.forget"));
        actions::popup(&b, x, y, &menu, &group);
    });
    button.add_controller(click);
}

/// Fill the Saved searches, Categories and Tags groups (after one is added or removed).
fn rebuild_lists() {
    let Some(ui) = ui() else { return };
    let bx = ui.borrow().lists_box.clone();
    while let Some(c) = bx.first_child() {
        bx.remove(&c);
    }
    {
        let mut u = ui.borrow_mut();
        u.nav_items.retain(|id, _| !id.starts_with("cat:") && !id.starts_with("tag:") && !id.starts_with(SAVED));
        u.nav_counts.retain(|id, _| !id.starts_with("cat:") && !id.starts_with("tag:"));
        u.only_when_used.clear();
    }
    let add = |filter: Filter, lead: Lead, menu: bool| {
        let id = filter.id();
        let title = filter.title();
        let (b, count) = nav_button(&id, &title, lead, &title, true);
        if menu {
            actions::attach_list_menu(&b, &filter);
        }
        bx.append(&b);
        let mut u = ui.borrow_mut();
        if let Some(c) = count {
            u.nav_counts.insert(id.clone(), c);
        }
        if !menu {
            b.set_visible(false);
            u.only_when_used.insert(id.clone(), b.clone());
        }
        u.nav_items.insert(id, b);
    };

    group_heading(&bx, "Saved searches", "discover", Some(("Save a search", add_saved_search)));
    for q in prefs::get().saved_searches {
        let id = format!("{SAVED}{q}");
        let (b, _) =
            nav_button(&id, &q, Lead::Icon("document-open-recent-symbolic"), &format!("Search the sites for “{q}”"), false);
        attach_saved_menu(&b, &q);
        bx.append(&b);
        ui.borrow_mut().nav_items.insert(id, b);
    }

    group_heading(&bx, "Categories", "categories", Some(("New category", || actions::edit_category_dialog(None))));
    for (i, name) in store::categories().keys().enumerate() {
        add(Filter::Category(Some(name.clone())), Lead::Dot(CATEGORY_SWATCHES[i % CATEGORY_SWATCHES.len()]), true);
    }
    add(Filter::Category(None), Lead::Icon("folder-symbolic"), false);

    group_heading(&bx, "Tags", "tags", Some(("New tag", actions::new_tag_dialog)));
    for name in store::tags() {
        add(Filter::Tag(Some(name.clone())), Lead::Icon("tag-symbolic"), true);
    }
    add(Filter::Tag(None), Lead::Icon("tag-symbolic"), false);

    // Keep the highlight on the current page (or fall back if it was removed).
    let current = ui.borrow().current.clone();
    if ui.borrow().nav_items.contains_key(&current) {
        mark_active(&current);
    } else if Filter::from_id(&current).is_some_and(|f| matches!(f, Filter::Category(Some(_)) | Filter::Tag(Some(_)))) {
        navigate("home");
    } else if current.starts_with(SAVED) {
        navigate("search");
    }
    if let Some(u) = live::latest() {
        update_counts(&u);
    }
    apply_compact(COMPACT.with(|c| c.get()));
}

fn update_counts(u: &crate::engine::Update) {
    let Some(ui) = ui() else { return };
    let counts: Vec<(String, gtk::Label)> = ui.borrow().nav_counts.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let used: Vec<(String, gtk::Button)> = ui.borrow().only_when_used.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let metas: Vec<(&crate::engine::Status, store::Meta)> = u.torrents.iter().map(|t| (t, store::meta(&t.id))).collect();
    let count = |id: &str| Filter::from_id(id).map(|f| metas.iter().filter(|(s, m)| f.matches(s, m)).count()).unwrap_or(0);
    let compact = COMPACT.with(|c| c.get());
    for (id, label) in counts {
        let n = count(&id);
        label.set_text(&if n > 0 { n.to_string() } else { String::new() });
        label.set_visible(n > 0 && !compact);
        if id == "errored" {
            label.add_css_class("badge");
        }
    }
    // Uncategorized and Untagged only mean something once there are categories or tags.
    let has_categories = !store::categories().is_empty();
    let has_tags = !store::tags().is_empty();
    for (id, button) in used {
        let named = if id.starts_with("cat:") { has_categories } else { has_tags };
        button.set_visible(named && count(&id) > 0);
    }
}

/// Ctrl+K and the header's field: search the sites.
pub fn open_search() {
    if current() != "search" && !current().starts_with(SAVED) {
        navigate("search");
    }
    sections::search::focus_query();
}

/// Ctrl+F: filter the transfers list (going to it first if need be), or on Search,
/// go to the search field.
fn find_in_page() {
    if current() == "search" || current().starts_with(SAVED) {
        sections::search::focus_query();
        return;
    }
    if !is_transfers() {
        navigate("all");
    }
    transfers::focus_search();
}

/// The overall speeds, kept current.
fn header_readout() -> gtk::Box {
    let readout = widgets::hbox(6);
    readout.add_css_class("header-rate");
    readout.set_valign(gtk::Align::Center);
    readout.set_margin_end(6);
    let down = widgets::label("", "tnum");
    let up = widgets::label("", "tnum");
    readout.append(&gtk::Image::from_icon_name("go-down-symbolic"));
    readout.append(&down);
    let gap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    gap.add_css_class("rate-gap");
    readout.append(&gap);
    readout.append(&gtk::Image::from_icon_name("go-up-symbolic"));
    readout.append(&up);
    let r = readout.clone();
    live::on_tick(&readout, move |u| {
        down.set_text(&fmt::net_rate(u.stats.dl_rate as f64));
        up.set_text(&fmt::net_rate(u.stats.ul_rate as f64));
        r.set_tooltip_text(Some(&format!(
            "Downloading at {}, uploading at {}. Home has the connection's health.",
            fmt::net_rate(u.stats.dl_rate as f64),
            fmt::net_rate(u.stats.ul_rate as f64)
        )));
    });
    readout
}

/// Every keyboard shortcut, for the shortcuts dialog and Appearance.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "O"], "Add a .torrent file"),
    (&["Ctrl", "U"], "Add a magnet link"),
    (&["Ctrl", "K"], "Search the sites for torrents"),
    (&["Ctrl", "F"], "Filter the list"),
    (&["Space"], "Pause or resume the selection"),
    (&["Delete"], "Remove the selection"),
    (&["Ctrl", "C"], "Copy magnet links of the selection"),
    (&["Ctrl", "A"], "Select every torrent in view"),
    (&["Esc"], "Clear the search"),
    (&["Ctrl", "B"], "Collapse or expand the sidebar"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Show these shortcuts (or ?)"),
    (&["Ctrl", "W"], "Close the window"),
    (&["Ctrl", "Q"], "Quit"),
];

pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::keycaps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls without a scrollbar, which would cover the key caps.
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(560)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

pub fn current() -> String {
    ui().map(|u| u.borrow().current.clone()).unwrap_or_default()
}

/// The layer over the window, for toasts and the settings dialog.
pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

/// A narrow window lays its pages out for a half-screen tile.
fn set_narrow(narrow: bool) {
    NARROW.with(|n| n.set(narrow));
    let Some(ui) = ui() else { return };
    let pages: Vec<gtk::ScrolledWindow> = ui.borrow().pages.values().cloned().collect();
    for page in pages {
        mark_page(&page, narrow);
    }
    transfers::set_narrow(narrow);
    home::set_narrow(narrow);
    for w in &ui.borrow().wide_only {
        w.set_visible(!narrow);
    }
}

/// The sidebar shows only icons: headings give way to short bars in their colour.
fn apply_compact(compact: bool) {
    COMPACT.with(|c| c.set(compact));
    let Some(ui) = ui() else { return };
    let nav = ui.borrow().nav.clone();
    sidebar::set_compact(&nav, compact);
    fn walk(w: &gtk::Widget, compact: bool) {
        if w.has_css_class("compact-show") {
            w.set_visible(compact);
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            walk(&c, compact);
            child = c.next_sibling();
        }
    }
    walk(nav.upcast_ref(), compact);
}

pub fn toggle_sidebar() {
    prefs::update(|p| p.sidebar_collapsed = !p.sidebar_collapsed);
    apply_compact(NARROW.with(|n| n.get()) || prefs::get().sidebar_collapsed);
}

fn mark_page(page: &gtk::ScrolledWindow, narrow: bool) {
    if let Some(body) = page.child().and_then(|v| v.first_child()) {
        if narrow {
            body.add_css_class("narrow");
        } else {
            body.remove_css_class("narrow");
        }
    }
}

fn ensure_built(id: &'static str) {
    let Some(ui) = ui() else { return };
    if ui.borrow().pages.contains_key(id) {
        return;
    }
    let section = {
        let u = ui.borrow();
        u.sections.iter().find(|s| s.id == id).map(|s| (s.id, s.build, s.fill))
    };
    let Some((sid, build, fill)) = section else { return };
    let page = widgets::page(sid);
    if fill {
        page.fill();
    }
    build(&page);
    mark_page(&page.root, NARROW.with(|n| n.get()));
    if sid == "home" {
        home::set_narrow(NARROW.with(|n| n.get()));
    }
    let stack = ui.borrow().stack.clone();
    stack.add_named(&page.root, Some(sid));
    ui.borrow_mut().pages.insert(sid, page.root);
}

fn mark_active(id: &str) {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    for (k, b) in &u.nav_items {
        if k == id {
            b.add_css_class("active");
        } else {
            b.remove_css_class("active");
        }
    }
}

pub fn is_transfers() -> bool {
    ui().is_some_and(|u| Filter::from_id(&u.borrow().current).is_some())
}

pub fn navigate(id: &str) {
    let Some(ui) = ui() else { return };
    // Settings pages open in the dialog, over whatever is showing.
    if settings_dialog::is_settings(id) {
        settings_dialog::open(Some(id));
        if !ui.borrow().current.is_empty() {
            return;
        }
    }
    let mut crumb = String::new();
    let saved = id.strip_prefix(SAVED).map(String::from);
    let (page_id, nav_id): (&'static str, String) = if let Some(f) = Filter::from_id(id) {
        ensure_built("transfers");
        transfers::set_filter(f.clone());
        crumb = transfers::filter_title(&f);
        ("transfers", f.id())
    } else if let Some(q) = &saved {
        crumb = q.clone();
        ("search", id.to_string())
    } else {
        let found = ui
            .borrow()
            .sections
            .iter()
            .find(|s| s.id == id && s.id != "transfers" && !settings_dialog::is_settings(s.id))
            .map(|s| s.id);
        match found {
            Some(sid) => (sid, sid.to_string()),
            None => ("home", "home".to_string()),
        }
    };
    ensure_built(page_id);
    mark_active(&nav_id);
    {
        let mut u = ui.borrow_mut();
        if crumb.is_empty() {
            crumb = u.sections.iter().find(|s| s.id == page_id).map(|s| s.title.to_string()).unwrap_or_default();
        }
        chrome::set_title(&u.window, "Torrents", &crumb);
        u.stack.set_visible_child_name(page_id);
        u.find.set_visible(page_id != "search");
        u.current = nav_id.clone();
    }
    // A saved search runs now, but the app reopens on Search, not on a new search.
    let last = if saved.is_some() { "search".to_string() } else { nav_id };
    prefs::update(|p| p.last_section = last);
    if let Some(q) = saved {
        sections::search::run_query(&q);
    }
}

/// Show a short message at the bottom of the window.
pub fn toast(message: &str) {
    let Some(ui) = ui() else {
        eprintln!("nyx-torrents: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bx.add_css_class("toast");
    bx.append(&label);
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    bx.set_can_target(false);
    overlay.add_overlay(&bx);
    glib::timeout_add_local_once(std::time::Duration::from_millis(3500), move || {
        overlay.remove_overlay(&bx);
    });
}

/// A desktop notification (for when the window is hidden or unfocused).
pub fn notify(title: &str, body: &str) {
    let Some(ui) = ui() else { return };
    let (app, window) = {
        let u = ui.borrow();
        (u.app.clone(), u.window.clone())
    };
    if window.is_visible() && window.is_active() {
        toast(&format!("{title}: {body}"));
        return;
    }
    let n = gio::Notification::new(title);
    n.set_body(Some(body));
    n.set_icon(&gio::ThemedIcon::new("io.github.design_nexus.nyx.Torrents"));
    app.send_notification(None, &n);
}
