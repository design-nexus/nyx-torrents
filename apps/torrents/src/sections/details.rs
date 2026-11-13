//! The details pane under the torrent list: General (pieces bar and figures),
//! Trackers, Peers, Files and Speed, for the first selected torrent.

use crate::engine::{Command, Details, FileEntry, Peer, Tracker, Update};
use crate::graph::{self, Scale, Series, Tone};
use crate::table::{Choice, Column, OnChoice, Table};
use crate::theme::{self, Rgb};
use crate::widgets;
use crate::{actions, fmt, live, model, paths, prefs};
use gtk::pango;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::transfers;

const TABS: &[(&str, &str)] =
    &[("general", "General"), ("trackers", "Trackers"), ("peers", "Peers"), ("files", "Files"), ("speed", "Speed")];

/// libtorrent file priorities for Skip · Normal · High · Maximum.
const PRIORITIES: &[u8] = &[0, 4, 6, 7];

struct Pane {
    stack: gtk::Stack,
    tabs: Vec<(&'static str, gtk::Button)>,
    title: gtk::Label,
    placeholder: gtk::Stack,
    pieces: Rc<RefCell<Vec<u8>>>,
    pieces_area: gtk::DrawingArea,
    figures: Vec<gtk::Label>,
    facts: Vec<gtk::Label>,
    trackers: Table<Tracker>,
    peers: Table<Peer>,
    files: Table<(usize, FileEntry)>,
    speed_box: gtk::Box,
    id: Option<String>,
    graph_for: Option<String>,
}

thread_local! {
    static PANE: RefCell<Option<Rc<RefCell<Pane>>>> = const { RefCell::new(None) };
}

fn pane() -> Option<Rc<RefCell<Pane>>> {
    PANE.with(|p| p.borrow().clone())
}

const FIGURES: &[&str] = &[
    "Progress",
    "Downloaded",
    "Uploaded",
    "Ratio",
    "Down speed",
    "Up speed",
    "ETA",
    "Seeds",
    "Peers",
    "Availability",
    "Time active",
    "Seeding time",
    "Size",
    "Pieces",
    "Added",
    "Completed",
];

const FACTS: &[&str] = &["Save path", "Info hash v1", "Info hash v2", "Comment", "Created by", "Tracker"];

fn fact_row(key: &str) -> (gtk::Box, gtk::Label) {
    let row = widgets::hbox(12);
    row.add_css_class("fact-row");
    let k = widgets::label(key, "kv-key");
    k.set_width_chars(12);
    k.set_xalign(0.0);
    let v = widgets::label("–", "fact-value");
    v.set_selectable(true);
    v.set_hexpand(true);
    v.set_wrap(true);
    v.set_wrap_mode(pango::WrapMode::WordChar);
    v.set_xalign(0.0);
    row.append(&k);
    row.append(&v);
    (row, v)
}

fn tracker_status(t: &Tracker) -> String {
    match t.status {
        1 => "Working".into(),
        2 => "Updating…".into(),
        3 => "Not working".into(),
        _ => "Not contacted".into(),
    }
}

fn count(v: i32) -> String {
    if v < 0 { "–".into() } else { v.to_string() }
}

fn build_tables() -> (Table<Tracker>, Table<Peer>, Table<(usize, FileEntry)>) {
    let trackers = Table::new(
        vec![
            Column::text("URL", 0, |t: &Tracker| t.url.clone()).sorted(|a, b| a.url.cmp(&b.url)),
            Column::text("Status", 110, tracker_status),
            Column::num("Seeds", 70, |t| count(t.seeds), |a, b| a.seeds.cmp(&b.seeds)),
            Column::num("Peers", 70, |t| count(t.peers), |a, b| a.peers.cmp(&b.peers)),
            Column::text("Message", 180, |t: &Tracker| t.message.clone()),
            Column::num(
                "Next",
                70,
                |t| if t.next_announce < 0 { "–".into() } else { fmt::duration(t.next_announce as f64) },
                |a, b| a.next_announce.cmp(&b.next_announce),
            ),
            Column::text("Tier", 50, |t: &Tracker| t.tier.to_string()),
            Column::num("Downloaded", 96, |t| count(t.downloaded), |a, b| a.downloaded.cmp(&b.downloaded)),
        ],
        "No trackers. This torrent finds peers through DHT and peer exchange.",
    );
    let peers = Table::new(
        vec![
            Column::text("Address", 170, |p: &Peer| p.address.clone()),
            Column::text("Client", 0, |p: &Peer| p.client.clone()).sorted(|a, b| a.client.cmp(&b.client)),
            Column::num(
                "Progress",
                76,
                |p| format!("{:.0}%", p.progress * 100.0),
                |a, b| a.progress.partial_cmp(&b.progress).unwrap_or(std::cmp::Ordering::Equal),
            ),
            Column::num(
                "Down",
                104,
                |p| if p.dl_rate > 0 { fmt::net_rate(p.dl_rate as f64) } else { String::new() },
                |a, b| a.dl_rate.cmp(&b.dl_rate),
            ),
            Column::num(
                "Up",
                104,
                |p| if p.ul_rate > 0 { fmt::net_rate(p.ul_rate as f64) } else { String::new() },
                |a, b| a.ul_rate.cmp(&b.ul_rate),
            ),
            Column::text("Flags", 64, |p: &Peer| p.flags.clone()),
            Column::text("Via", 60, |p: &Peer| p.connection.clone()),
            Column::num("Downloaded", 96, |p| fmt::bytes(p.downloaded as f64), |a, b| a.downloaded.cmp(&b.downloaded)),
            Column::num("Uploaded", 96, |p| fmt::bytes(p.uploaded as f64), |a, b| a.uploaded.cmp(&b.uploaded)),
        ],
        "No peers connected.",
    );
    let on_change: OnChoice = Rc::new(|key, choice| {
        let Some(p) = pane() else { return };
        let Some(id) = p.borrow().id.clone() else { return };
        let (Ok(index), Some(prio)) = (key.parse::<usize>(), PRIORITIES.get(choice as usize).copied()) else { return };
        let Some(details) = live::latest().and_then(|u| u.details.clone()).filter(|d| d.id == id) else { return };
        let mut prios: Vec<u8> = details.files.iter().map(|f| if f.priority == 255 { 0 } else { f.priority }).collect();
        if index < prios.len() {
            prios[index] = prio;
            live::send(Command::FilePriorities(id, prios));
        }
    });
    let files = Table::new(
        vec![
            Column::text("Name", 0, |f: &(usize, FileEntry)| f.1.path.clone()).sorted(|a, b| a.1.path.cmp(&b.1.path)),
            Column::num("Size", 88, |f| fmt::bytes(f.1.size as f64), |a, b| a.1.size.cmp(&b.1.size)),
            Column::num(
                "Progress",
                76,
                |f| {
                    if f.1.size <= 0 { "100%".into() } else { format!("{:.1}%", f.1.done as f64 / f.1.size as f64 * 100.0) }
                },
                |a, b| (a.1.done * 1000 / a.1.size.max(1)).cmp(&(b.1.done * 1000 / b.1.size.max(1))),
            ),
            Column::num(
                "Remaining",
                96,
                |f| if f.1.priority == 0 { "–".into() } else { fmt::bytes((f.1.size - f.1.done).max(0) as f64) },
                |a, b| (a.1.size - a.1.done).cmp(&(b.1.size - b.1.done)),
            ),
            Column {
                title: "Priority",
                width: 140,
                numeric: false,
                text: |_| String::new(),
                compare: Some(|a, b| a.1.priority.cmp(&b.1.priority)),
                choice: Some(Choice {
                    labels: &["Skip", "Normal", "High", "Maximum"],
                    index: |f| match f.1.priority {
                        0 => 0,
                        1..=4 => 1,
                        5 | 6 => 2,
                        _ => 3,
                    },
                    on_change,
                }),
                lead: None,
                button: None,
            },
        ],
        "The file list appears once the torrent's metadata arrives.",
    );
    (trackers, peers, files)
}

pub fn build() -> gtk::Widget {
    let root = widgets::vbox(0);
    root.add_css_class("details-pane");
    root.set_size_request(-1, 120);

    let head = widgets::hbox(12);
    head.add_css_class("details-head");
    let title = widgets::label("", "details-title");
    title.set_hexpand(true);
    title.set_ellipsize(pango::EllipsizeMode::End);
    head.append(&title);
    let seg = widgets::hbox(0);
    seg.add_css_class("segmented");
    seg.set_valign(gtk::Align::Center);
    let mut tabs = Vec::new();
    for (id, label) in TABS {
        let b = gtk::Button::with_label(label);
        let id2 = *id;
        b.connect_clicked(move |_| select_tab(id2));
        seg.append(&b);
        tabs.push((*id, b));
    }
    head.append(&seg);
    root.append(&head);

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 120 });

    // General
    let general = widgets::vbox(12);
    general.add_css_class("details-general");
    let pieces: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
    let pieces_area = gtk::DrawingArea::new();
    pieces_area.set_content_height(14);
    pieces_area.set_hexpand(true);
    pieces_area.add_css_class("pieces-bar");
    pieces_area.set_tooltip_text(Some("Pieces: downloaded, downloading and missing"));
    {
        let pieces = pieces.clone();
        pieces_area.set_draw_func(move |_, cr, w, h| draw_pieces(cr, w as f64, h as f64, &pieces.borrow()));
    }
    general.append(&pieces_area);
    let (flow, figures) = widgets::kv_flow(FIGURES);
    general.append(&flow);
    let facts_box = widgets::vbox(4);
    let mut facts = Vec::new();
    for k in FACTS {
        let (row, v) = fact_row(k);
        facts_box.append(&row);
        if matches!(*k, "Save path" | "Info hash v1" | "Info hash v2") {
            v.add_css_class("mono");
        }
        facts.push(v);
    }
    general.append(&facts_box);
    let general_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&general)
        .build();
    stack.add_named(&general_scroll, Some("general"));

    let (trackers, peers, files) = build_tables();

    // Trackers, with a field to add one.
    let tr_box = widgets::vbox(8);
    tr_box.append(&trackers.root);
    let add_row = widgets::hbox(8);
    add_row.add_css_class("details-add-row");
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some("https://tracker.example/announce"));
    entry.add_css_class("mono");
    entry.set_hexpand(true);
    let add = gtk::Button::with_label("Add tracker");
    let e2 = entry.clone();
    let do_add = move || {
        let url = e2.text().trim().to_string();
        let Some(id) = pane().and_then(|p| p.borrow().id.clone()) else { return };
        if url.is_empty() {
            return;
        }
        live::send(Command::AddTracker(id, url));
        e2.set_text("");
    };
    let d2 = do_add.clone();
    add.connect_clicked(move |_| d2());
    entry.connect_activate(move |_| do_add());
    add_row.append(&entry);
    add_row.append(&add);
    tr_box.append(&add_row);
    // Right-click a tracker to remove it or copy its URL.
    {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let view = trackers.view.clone();
        click.connect_pressed(move |_, _, x, y| {
            let Some(w) = view.pick(x, y, gtk::PickFlags::DEFAULT) else { return };
            let mut cur = Some(w);
            while let Some(c) = cur {
                if let Some(l) = c.downcast_ref::<gtk::Label>()
                    && l.text().contains("://")
                {
                    actions::tracker_menu(&view, x, y, &l.text());
                    return;
                }
                if let Some(l) = c.first_child().and_downcast::<gtk::Label>()
                    && l.text().contains("://")
                {
                    actions::tracker_menu(&view, x, y, &l.text());
                    return;
                }
                cur = c.parent();
            }
        });
        trackers.view.add_controller(click);
    }
    stack.add_named(&tr_box, Some("trackers"));
    stack.add_named(&peers.root, Some("peers"));
    stack.add_named(&files.root, Some("files"));

    let speed_box = widgets::vbox(0);
    speed_box.add_css_class("details-speed");
    stack.add_named(&speed_box, Some("speed"));

    let placeholder = gtk::Stack::new();
    let hint = widgets::label("Select a torrent to see its details.", "empty-state");
    hint.set_xalign(0.5);
    placeholder.add_named(&hint, Some("none"));
    placeholder.add_named(&stack, Some("pane"));
    placeholder.set_visible_child_name("none");
    placeholder.set_vexpand(true);
    root.append(&placeholder);

    let p = Pane {
        stack,
        tabs,
        title,
        placeholder,
        pieces,
        pieces_area,
        figures,
        facts,
        trackers,
        peers,
        files,
        speed_box,
        id: None,
        graph_for: None,
    };
    PANE.with(|cell| *cell.borrow_mut() = Some(Rc::new(RefCell::new(p))));
    select_tab(&prefs::get().details_tab);
    live::on_tick(&root, refresh);
    root.upcast()
}

fn select_tab(id: &str) {
    let Some(p) = pane() else { return };
    let id = TABS.iter().find(|t| t.0 == id).map(|t| t.0).unwrap_or("general");
    {
        let p = p.borrow();
        p.stack.set_visible_child_name(id);
        for (tid, b) in &p.tabs {
            if *tid == id {
                b.add_css_class("selected");
            } else {
                b.remove_css_class("selected");
            }
        }
    }
    if prefs::get().details_tab != id {
        prefs::update(|pr| pr.details_tab = id.to_string());
    }
    let current = p.borrow().id.clone();
    live::send(Command::Select(current, id == "peers"));
    ensure_graph();
}

/// Show `id` (or nothing).
pub fn show(id: Option<String>) {
    let Some(p) = pane() else { return };
    {
        let mut p = p.borrow_mut();
        if p.id == id {
            return;
        }
        p.id = id.clone();
        p.placeholder.set_visible_child_name(if id.is_some() { "pane" } else { "none" });
        if id.is_none() {
            p.title.set_text("");
        }
        p.trackers.clear();
        p.peers.clear();
        p.files.clear();
        p.pieces.borrow_mut().clear();
    }
    let peers = prefs::get().details_tab == "peers";
    live::send(Command::Select(id, peers));
    ensure_graph();
    if let Some(u) = live::latest() {
        refresh(&u);
    }
}

/// The Speed tab's graph follows the selected torrent.
fn ensure_graph() {
    let Some(p) = pane() else { return };
    let (id, graph_for, bx, visible) = {
        let p = p.borrow();
        (p.id.clone(), p.graph_for.clone(), p.speed_box.clone(), p.stack.visible_child_name().as_deref() == Some("speed"))
    };
    if !visible || id == graph_for {
        return;
    }
    while let Some(c) = bx.first_child() {
        bx.remove(&c);
    }
    if let Some(id) = &id {
        let g = graph::graph(
            vec![
                Series::new(format!("t.{id}.dl"), "Download", Tone::Accent),
                Series::new(format!("t.{id}.ul"), "Upload", Tone::Second),
            ],
            Scale::Auto { floor: 16.0 * 1024.0 },
            fmt::net_rate,
            150,
        );
        bx.append(&g.root);
    }
    p.borrow_mut().graph_for = id;
}

fn refresh(u: &Update) {
    let Some(p) = pane() else { return };
    let Some(id) = p.borrow().id.clone() else { return };
    let Some(row) = transfers::row(&id) else {
        return;
    };
    let s = &row.s;
    let details: Option<&Details> = u.details.as_ref().filter(|d| d.id == id);
    let p = p.borrow();
    p.title.set_text(&s.name);
    let tab = p.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default();

    if tab == "general" {
        let ratio = model::ratio(s);
        let values = [
            format!("{:.1}%", s.progress * 100.0),
            fmt::bytes(s.downloaded as f64),
            fmt::bytes(s.uploaded as f64),
            transfers::ratio_text(ratio),
            fmt::net_rate(s.dl_rate as f64),
            fmt::net_rate(s.ul_rate as f64),
            model::eta(s).map(|e| fmt::duration(e as f64)).unwrap_or_else(|| "∞".into()),
            transfers::peers_text(s.seeds, s.seeds_total),
            transfers::peers_text(s.peers, s.peers_total),
            if s.availability >= 0.0 { format!("{:.2}", s.availability) } else { "–".into() },
            fmt::duration(s.active_secs as f64),
            fmt::duration(s.seeding_secs as f64),
            if s.has_metadata { fmt::bytes(s.size as f64) } else { "–".into() },
            if s.num_pieces > 0 { format!("{} / {}", s.pieces_done, s.num_pieces) } else { "–".into() },
            transfers::date_text(s.added),
            transfers::date_text(s.completed),
        ];
        for (l, v) in p.figures.iter().zip(values.iter()) {
            if l.text() != v.as_str() {
                l.set_text(v);
            }
        }
        let d = details.cloned().unwrap_or_default();
        let facts = [
            paths::pretty(std::path::Path::new(&s.save_path)),
            if d.hash_v1.is_empty() { "–".into() } else { d.hash_v1.clone() },
            if d.hash_v2.is_empty() { "–".into() } else { d.hash_v2.clone() },
            if d.comment.is_empty() { "–".into() } else { d.comment.clone() },
            if d.creator.is_empty() { "–".into() } else { d.creator.clone() },
            if s.tracker.is_empty() { "–".into() } else { s.tracker.clone() },
        ];
        for (l, v) in p.facts.iter().zip(facts.iter()) {
            if l.text() != v.as_str() {
                l.set_text(v);
            }
        }
        if let Some(d) = details {
            *p.pieces.borrow_mut() = d.pieces.clone();
            p.pieces_area.queue_draw();
        }
    }
    let Some(d) = details else { return };
    match tab.as_str() {
        "trackers" => p.trackers.set(d.trackers.iter().map(|t| (t.url.clone(), t.clone())).collect()),
        "peers" => p.peers.set(d.peers.iter().map(|x| (x.address.clone(), x.clone())).collect()),
        "files" => p.files.set(
            d.files.iter().enumerate().filter(|(_, f)| f.priority != 255).map(|(i, f)| (i.to_string(), (i, f.clone()))).collect(),
        ),
        _ => {}
    }
}

/// Downloaded pieces in `accent`, ones in flight in a lighter accent, missing ones on the trough.
fn draw_pieces(cr: &gtk::cairo::Context, w: f64, h: f64, pieces: &[u8]) {
    let pal = theme::palette();
    let accent = Rgb::hex(&pal.accent);
    let muted = Rgb::hex(&pal.muted);
    let r = 4.0_f64.min(h / 2.0);
    // Rounded clip for the whole bar.
    cr.new_sub_path();
    cr.arc(w - r, r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(w - r, h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(r, h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
    cr.arc(r, r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
    cr.close_path();
    cr.clip();
    cr.set_source_rgba(muted.0, muted.1, muted.2, 0.8);
    cr.paint().ok();
    let n = pieces.len();
    if n == 0 {
        return;
    }
    // One column per pixel: its shade is the share of pieces there that are done.
    let cols = w.max(1.0) as usize;
    for x in 0..cols {
        let a = x * n / cols;
        let b = (((x + 1) * n) / cols).max(a + 1).min(n);
        let slice = &pieces[a..b];
        let have = slice.iter().filter(|p| **p == 2).count() as f64 / slice.len() as f64;
        let busy = slice.contains(&1);
        if have > 0.0 {
            cr.set_source_rgba(accent.0, accent.1, accent.2, 0.35 + 0.65 * have);
            cr.rectangle(x as f64, 0.0, 1.0, h);
            cr.fill().ok();
        } else if busy {
            cr.set_source_rgba(accent.0, accent.1, accent.2, 0.3);
            cr.rectangle(x as f64, 0.0, 1.0, h);
            cr.fill().ok();
        }
    }
}
