//! The main window: the header (the app and where you are, the sidebar toggle; the
//! overall speeds and connection, the slow-mode chip, search and settings at the end),
//! the navigation sidebar (transfer filters with live counts, categories, tags, then
//! Search) and a stack of pages built the first time they're shown. Every transfer
//! filter shows the same Transfers page. The settings pages open in the settings
//! dialog instead (see `settings_dialog`).

use crate::model::{Filter, STATUS_FILTERS};
use crate::sections::{self, Section, transfers};
use crate::widgets;
use crate::{actions, fmt, live, prefs, settings_dialog, store, theme};
use gtk::prelude::*;
use nyx_ui::chrome;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

struct Ui {
    app: gtk::Application,
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    /// Nav buttons by id: filter ids ("all", "cat:Movies"…) and page ids.
    nav_items: HashMap<String, gtk::Button>,
    nav_counts: HashMap<String, gtk::Label>,
    categories_box: gtk::Box,
    tags_box: gtk::Box,
    pages: HashMap<&'static str, gtk::ScrolledWindow>,
    sections: Vec<Section>,
    current: String,
    overlay: gtk::Overlay,
    compact_hide: Vec<gtk::Widget>,
    nav: gtk::Box,
    header: chrome::AppHeader,
    /// The speeds and connection, at the header's end.
    readout: gtk::Label,
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
    let start = section.map(String::from).unwrap_or_else(|| nyx_theme::start_page("all", &prefs::get().last_section));
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

fn nav_button(id: &str, title: &str, icon: &str, tooltip: &str, count: bool) -> (gtk::Button, Option<gtk::Label>, gtk::Label) {
    let button = gtk::Button::new();
    button.add_css_class("nav-item");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(&gtk::Image::from_icon_name(icon));
    let l = widgets::label(title, "nav-label");
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&l);
    let readout = count.then(|| {
        let r = widgets::label("", "nav-readout");
        r.add_css_class("tnum");
        content.append(&r);
        r
    });
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(tooltip));
    let id = id.to_string();
    button.connect_clicked(move |_| navigate(&id));
    (button, readout, l)
}

fn group_heading(title: &str, add: Option<(&str, fn())>) -> (gtk::Box, gtk::Label) {
    let row = widgets::hbox(4);
    row.add_css_class("nav-group-row");
    let g = widgets::label(&title.to_uppercase(), "nav-group");
    g.set_hexpand(true);
    row.append(&g);
    if let Some((tip, f)) = add {
        let b = gtk::Button::from_icon_name("list-add-symbolic");
        b.add_css_class("flat");
        b.add_css_class("nav-add");
        b.set_tooltip_text(Some(tip));
        b.connect_clicked(move |_| f());
        row.append(&b);
    }
    (row, g)
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
    let mut compact_hide: Vec<gtk::Widget> = Vec::new();

    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let mut nav_items = HashMap::new();
    let mut nav_counts = HashMap::new();

    // Transfers
    let transfers_box = widgets::vbox(0);
    let (h, g) = group_heading("Transfers", None);
    h.add_css_class("first");
    compact_hide.push(g.clone().upcast());
    transfers_box.append(&h);
    for (id, title, icon) in STATUS_FILTERS {
        let (b, count, label) = nav_button(id, title, icon, title, true);
        if let Some(c) = count {
            compact_hide.push(c.clone().upcast());
            nav_counts.insert(id.to_string(), c);
        }
        compact_hide.push(label.upcast());
        transfers_box.append(&b);
        nav_items.insert(id.to_string(), b);
    }
    list.append(&transfers_box);

    // Categories and tags are filled by `rebuild_lists`.
    let categories_box = widgets::vbox(0);
    list.append(&categories_box);
    let tags_box = widgets::vbox(0);
    list.append(&tags_box);

    let mut last_group = "";
    // The Transfers page has no nav item of its own: the filters above lead to it.
    // Settings pages live in the settings dialog.
    for s in sections.iter().filter(|s| !s.group.is_empty() && s.group != settings_dialog::GROUP) {
        if s.group != last_group {
            let (h, g) = group_heading(s.group, None);
            compact_hide.push(g.upcast());
            list.append(&h);
            last_group = s.group;
        }
        let (b, _, label) = nav_button(s.id, s.title, s.icon, s.description, false);
        compact_hide.push(label.upcast());
        list.append(&b);
        nav_items.insert(s.id.to_string(), b);
    }
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
    // The app and where you are, the sidebar toggle; the speeds and connection, the
    // slow-mode chip, search and Settings at the end.
    let header = chrome::app_header(&window, "torrents-magnet-symbolic", "Torrents");
    let collapse = widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    collapse.connect_clicked(|_| toggle_sidebar());
    header.start.append(&collapse);
    header.end.set_spacing(4);
    let readout = header_readout();
    header.end.append(&readout);
    header.end.append(&alt_speed_chip());
    let search = widgets::bar_button("system-search-symbolic", "Filter the list (Ctrl+F)");
    search.connect_clicked(|_| find());
    header.end.append(&search);
    header.end.append(&chrome::settings_button(|| settings_dialog::open(None)));

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
                find();
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
            let narrow = width > 0 && width < 980;
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
        categories_box,
        tags_box,
        pages: HashMap::new(),
        sections,
        current: String::new(),
        overlay,
        compact_hide,
        nav: nav.clone(),
        header,
        readout,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));
    rebuild_lists();
    store::on_lists_changed(rebuild_lists);
    live::on_update(update_counts);
}

fn free_space(path: &str) -> Option<u64> {
    let mut p = crate::paths::expand(path);
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a properly sized out-parameter.
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
    ok.then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

thread_local! {
    static ALT_CHIPS: RefCell<Vec<gtk::Button>> = const { RefCell::new(Vec::new()) };
}

/// The turtle chip: alternative speed limits on or off.
fn alt_speed_chip() -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("chip");
    b.add_css_class("alt-chip");
    let content = widgets::hbox(6);
    content.append(&gtk::Image::from_icon_name("torrents-turtle-symbolic"));
    content.append(&widgets::label("Slow", ""));
    b.set_child(Some(&content));
    b.connect_clicked(|_| actions::toggle_alt_speed());
    ALT_CHIPS.with(|c| c.borrow_mut().push(b.clone()));
    refresh_alt_speed();
    b
}

pub fn refresh_alt_speed() {
    let on = live::alt_active();
    let p = prefs::get();
    let tip = format!(
        "Alternative speed limits ({} down, {} up){}",
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

/// Fill the Categories and Tags groups (after one is added or removed).
fn rebuild_lists() {
    let Some(ui) = ui() else { return };
    let (cbox, tbox) = {
        let u = ui.borrow();
        (u.categories_box.clone(), u.tags_box.clone())
    };
    for bx in [&cbox, &tbox] {
        while let Some(c) = bx.first_child() {
            bx.remove(&c);
        }
    }
    {
        let mut u = ui.borrow_mut();
        u.nav_items.retain(|id, _| !id.starts_with("cat:") && !id.starts_with("tag:"));
        u.nav_counts.retain(|id, _| !id.starts_with("cat:") && !id.starts_with("tag:"));
    }
    let compact = COMPACT.with(|c| c.get());
    let add = |bx: &gtk::Box, filter: Filter, icon: &str, menu: bool| {
        let id = filter.id();
        let title = filter.title();
        let (b, count, label) = nav_button(&id, &title, icon, &title, true);
        label.set_visible(!compact);
        if menu {
            actions::attach_list_menu(&b, &filter);
        }
        bx.append(&b);
        let mut u = ui.borrow_mut();
        if let Some(c) = count {
            c.set_visible(!compact);
            u.compact_hide.push(c.clone().upcast());
            u.nav_counts.insert(id.clone(), c);
        }
        u.compact_hide.push(label.upcast());
        u.nav_items.insert(id, b);
    };

    let (h, g) = group_heading("Categories", Some(("New category", || actions::edit_category_dialog(None))));
    g.set_visible(!compact);
    h.set_visible(!compact);
    ui.borrow_mut().compact_hide.push(h.clone().upcast());
    cbox.append(&h);
    add(&cbox, Filter::Category(None), "folder-symbolic", false);
    for name in store::categories().keys() {
        add(&cbox, Filter::Category(Some(name.clone())), "folder-symbolic", true);
    }

    let (h, g) = group_heading("Tags", Some(("New tag", actions::new_tag_dialog)));
    g.set_visible(!compact);
    h.set_visible(!compact);
    ui.borrow_mut().compact_hide.push(h.clone().upcast());
    tbox.append(&h);
    add(&tbox, Filter::Tag(None), "tag-symbolic", false);
    for name in store::tags() {
        add(&tbox, Filter::Tag(Some(name.clone())), "tag-symbolic", true);
    }

    // Keep the highlight on the current filter (or fall back if it was removed).
    let current = ui.borrow().current.clone();
    if Filter::from_id(&current).is_some() {
        if ui.borrow().nav_items.contains_key(&current) {
            mark_active(&current);
        } else {
            navigate("all");
        }
    }
    if let Some(u) = live::latest() {
        update_counts(&u);
    }
    // New entries start expanded; centre their icons if the sidebar is icon-only.
    if COMPACT.with(|c| c.get()) {
        let nav = ui.borrow().nav.clone();
        centre_icons(nav.upcast_ref(), true);
    }
}

fn update_counts(u: &crate::engine::Update) {
    let Some(ui) = ui() else { return };
    let counts: Vec<(String, gtk::Label)> = ui.borrow().nav_counts.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let metas: Vec<(&crate::engine::Status, store::Meta)> = u.torrents.iter().map(|t| (t, store::meta(&t.id))).collect();
    for (id, label) in counts {
        let Some(f) = Filter::from_id(&id) else { continue };
        let n = metas.iter().filter(|(s, m)| f.matches(s, m)).count();
        label.set_text(&n.to_string());
    }
}

/// Ctrl+F: filter the transfers list (going to it first if need be).
fn find() {
    if !is_transfers() && current() != "search" {
        navigate("all");
    }
    if current() == "search" {
        sections::search::focus_query();
    } else {
        transfers::focus_search();
    }
}

/// The overall speeds and the connection, kept current.
fn header_readout() -> gtk::Label {
    let readout = widgets::label("", "header-status");
    readout.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    readout.set_margin_end(6);
    let r = readout.clone();
    live::on_tick(&readout, move |u| {
        let mut parts = vec![
            format!("↓ {}", fmt::net_rate(u.stats.dl_rate as f64)),
            format!("↑ {}", fmt::net_rate(u.stats.ul_rate as f64)),
            format!("Port {}", u.stats.listen_port),
        ];
        if prefs::get().dht {
            parts.push(format!("DHT {}", u.stats.dht_nodes));
        }
        if let Some(free) = free_space(&prefs::get().save_path) {
            parts.push(format!("{} free", fmt::bytes(free as f64)));
        }
        r.set_text(&parts.join(" · "));
        r.set_tooltip_text(Some(if u.stats.has_incoming {
            "Listening for incoming connections."
        } else {
            "Not listening: check the port and network interface in Connection."
        }));
    });
    readout
}

/// Every keyboard shortcut, for the shortcuts dialog and Appearance.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "O"], "Add a .torrent file"),
    (&["Ctrl", "U"], "Add a magnet link"),
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
    // A tiled half-screen header has no room for the speeds.
    ui.borrow().readout.set_visible(!narrow);
}

/// The sidebar shows only icons: hide the labels, centre the icons and the toggle.
fn apply_compact(compact: bool) {
    COMPACT.with(|c| c.set(compact));
    let Some(ui) = ui() else { return };
    let (hide, nav) = {
        let u = ui.borrow();
        (u.compact_hide.clone(), u.nav.clone())
    };
    if compact {
        nav.add_css_class("compact");
    } else {
        nav.remove_css_class("compact");
    }
    for w in hide {
        w.set_visible(!compact);
    }
    centre_icons(nav.upcast_ref(), compact);
}

fn centre_icons(w: &gtk::Widget, compact: bool) {
    if w.has_css_class("nav-item")
        && let Some(content) = w.downcast_ref::<gtk::Button>().and_then(|b| b.child())
    {
        content.set_halign(if compact { gtk::Align::Center } else { gtk::Align::Fill });
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        centre_icons(&c, compact);
        child = c.next_sibling();
    }
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
    let (page_id, nav_id): (&'static str, String) = if let Some(f) = Filter::from_id(id) {
        ensure_built("transfers");
        transfers::set_filter(f.clone());
        crumb = transfers::filter_title(&f);
        ("transfers", f.id())
    } else {
        let found = ui
            .borrow()
            .sections
            .iter()
            .find(|s| s.id == id && s.id != "transfers" && !settings_dialog::is_settings(s.id))
            .map(|s| s.id);
        match found {
            Some(sid) => (sid, sid.to_string()),
            None => {
                ensure_built("transfers");
                transfers::set_filter(Filter::All);
                crumb = transfers::filter_title(&Filter::All);
                ("transfers", "all".to_string())
            }
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
        u.header.set_subtitle(&crumb);
        u.stack.set_visible_child_name(page_id);
        u.current = nav_id.clone();
    }
    prefs::update(|p| p.last_section = nav_id);
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
