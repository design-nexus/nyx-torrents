//! Home: is it working? The speeds, how much has been shared, free space, a speed
//! graph, what's downloading, the connection's health and what finished lately.

use crate::engine::{Command, Status, Update};
use crate::model::{self, Filter};
use crate::theme::{self, Rgb};
use crate::widgets::{self, Page};
use crate::{actions, fmt, graph, live, paths, prefs, settings_dialog, store, window};
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::RefCell;
use std::rc::Rc;

/// How many torrents each list shows before "See all".
const DOWNLOADING_SHOWN: usize = 3;
const FINISHED_SHOWN: usize = 5;

struct Stat {
    value: gtk::Label,
    sub: gtk::Label,
}

struct Mini {
    bar: gtk::ProgressBar,
    line: gtk::Label,
    pause: gtk::Button,
}

struct ConnRow {
    key: gtk::Label,
    value: gtk::Label,
}

struct Finished {
    meta: gtk::Label,
    when: gtk::Label,
}

struct Home {
    title: gtk::Label,
    line: gtk::Label,
    pause_all: gtk::Button,
    stats: [Stat; 4],
    graph: gtk::DrawingArea,
    two: gtk::Box,
    downloading: gtk::Box,
    downloading_ids: Vec<String>,
    minis: Vec<Mini>,
    conn: [ConnRow; 4],
    finished: gtk::Box,
    finished_ids: Vec<String>,
    finished_rows: Vec<Finished>,
}

thread_local! {
    static HOME: RefCell<Option<Rc<RefCell<Home>>>> = const { RefCell::new(None) };
}

fn home() -> Option<Rc<RefCell<Home>>> {
    HOME.with(|h| h.borrow().clone())
}

// ---------- Small helpers ----------

pub fn free_space(path: &str) -> Option<u64> {
    let mut p = paths::expand(path);
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a properly sized out-parameter.
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
    ok.then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

/// "about 4 minutes", for the line under the title.
fn about(secs: i64) -> String {
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match secs {
        ..=89 => "a minute".into(),
        90..=3599 => plural((secs + 30) / 60, "minute"),
        3600..=86_399 => plural((secs + 1800) / 3600, "hour"),
        _ => plural((secs + 43_200) / 86_400, "day"),
    }
}

/// A torrent's name as a person would say it: "debian-13.2.0-amd64…" is "Debian".
fn short_name(name: &str) -> String {
    let cut = name.find(['-', '_', '.', '(', '[']).filter(|&i| i >= 3).unwrap_or(name.len());
    let short = name[..cut].trim();
    let short = if short.chars().count() > 32 { name.chars().take(32).collect::<String>() + "…" } else { short.to_string() };
    let mut chars = short.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// When a torrent finished: "Today 14:10", "Yesterday", "Mon", "3 Nov".
fn when(ts: i64) -> String {
    let (Ok(then), Ok(now)) = (glib::DateTime::from_unix_local(ts), glib::DateTime::now_local()) else { return String::new() };
    if ts <= 0 {
        return String::new();
    }
    let days = now.day_of_year() - then.day_of_year() + 365 * (now.year() - then.year());
    let format = match days {
        0 => "Today %H:%M",
        1 => "Yesterday",
        2..=6 => "%a",
        _ => "%e %b",
    };
    then.format(format).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn limit_text(kib: i32) -> String {
    if kib <= 0 { "No limit".into() } else { format!("Limit {}", fmt::rate(kib as f64 * 1024.0)) }
}

/// Whether an interface name looks like a VPN's.
fn looks_like_vpn(iface: &str) -> bool {
    ["wg", "tun", "tap", "proton", "mullvad", "nordlynx", "ppp"].iter().any(|p| iface.starts_with(p))
}

fn set_tone(label: &gtk::Label, tone: Option<&str>) {
    for t in ["ok", "bad", "accent"] {
        label.remove_css_class(t);
    }
    if let Some(t) = tone {
        label.add_css_class(t);
    }
}

fn section_head(title: &str, see_all: Option<&'static str>) -> gtk::Box {
    let head = widgets::hbox(8);
    head.add_css_class("home-head");
    let t = widgets::label(title, "home-head-title");
    t.set_hexpand(true);
    head.append(&t);
    if let Some(id) = see_all {
        let b = gtk::Button::with_label("See all");
        b.add_css_class("flat");
        b.add_css_class("link-button");
        b.connect_clicked(move |_| window::navigate(id));
        head.append(&b);
    }
    head
}

fn empty_card(text: &str) -> gtk::Box {
    let card = widgets::vbox(0);
    card.add_css_class("card");
    card.add_css_class("home-empty");
    card.append(&widgets::label(text, "dim"));
    card
}

// ---------- Build ----------

fn stat_card(icon: &str, key: &str) -> (gtk::Box, Stat) {
    let card = widgets::vbox(4);
    card.add_css_class("card");
    card.add_css_class("stat-card");
    let k = widgets::hbox(6);
    k.add_css_class("stat-key");
    k.append(&gtk::Image::from_icon_name(icon));
    k.append(&widgets::label(key, ""));
    card.append(&k);
    let value = widgets::label("–", "stat-value");
    value.add_css_class("tnum");
    value.set_ellipsize(pango::EllipsizeMode::End);
    card.append(&value);
    let sub = widgets::label("", "stat-sub");
    sub.set_ellipsize(pango::EllipsizeMode::Middle);
    // Four cards fit a half-screen window even with a long folder name.
    sub.set_max_width_chars(14);
    value.set_max_width_chars(10);
    card.append(&sub);
    (card, Stat { value, sub })
}

fn conn_row(list: &gtk::Box, icon: &str, settings: &'static str) -> ConnRow {
    if list.first_child().is_some() {
        let line = gtk::Separator::new(gtk::Orientation::Horizontal);
        line.add_css_class("row-line");
        list.append(&line);
    }
    let row = widgets::hbox(10);
    row.add_css_class("conn-row");
    let image = gtk::Image::from_icon_name(icon);
    image.add_css_class("conn-icon");
    row.append(&image);
    let key = widgets::label("", "conn-key");
    key.set_hexpand(true);
    key.set_ellipsize(pango::EllipsizeMode::End);
    row.append(&key);
    let value = widgets::label("", "conn-value");
    value.add_css_class("tnum");
    row.append(&value);
    row.set_tooltip_text(Some("Change this in Settings"));
    let click = gtk::GestureClick::new();
    click.connect_released(move |_, _, _, _| settings_dialog::open(Some(settings)));
    row.add_controller(click);
    list.append(&row);
    ConnRow { key, value }
}

fn speed_card() -> (gtk::Box, gtk::DrawingArea) {
    let card = widgets::vbox(10);
    card.add_css_class("card");
    card.add_css_class("speed-card");
    let head = widgets::hbox(14);
    head.append(&widgets::label("Speed", "home-card-title"));
    let legend = widgets::hbox(6);
    legend.add_css_class("speed-legend");
    for (class, text) in [("down", "Down"), ("up", "Up")] {
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("legend-dot");
        dot.add_css_class(class);
        dot.set_valign(gtk::Align::Center);
        legend.append(&dot);
        legend.append(&widgets::label(text, ""));
    }
    legend.set_hexpand(true);
    head.append(&legend);
    let area = gtk::DrawingArea::new();
    area.set_content_height(84);
    area.set_hexpand(true);
    let a = area.clone();
    let span =
        widgets::segmented(&[("5min", "5 min"), ("hour", "1 hour"), ("today", "Today")], &prefs::get().home_span, move |v| {
            prefs::update(|p| p.home_span = v);
            a.queue_draw();
        });
    head.append(&span);
    card.append(&head);
    area.set_draw_func(|area, cr, w, h| draw_speed(area, cr, w as f64, h as f64));
    area.set_has_tooltip(true);
    area.connect_query_tooltip(|area, x, _, _, tip| {
        let (bars, from, step) = speed_slices();
        let w = area.width() as f64;
        let i = ((x as f64 / w) * bars.len() as f64) as usize;
        let start = from + (i as f64 * step) as i64;
        let label = glib::DateTime::from_unix_local(start).ok().and_then(|d| d.format("%H:%M").ok()).map(|s| s.to_string());
        let text = match bars.get(i).copied().flatten() {
            Some((dl, ul)) => format!("Down {} · Up {}", fmt::net_rate(dl), fmt::net_rate(ul)),
            None => "Nothing recorded".into(),
        };
        tip.set_text(Some(&format!("{}{text}", label.map(|l| format!("{l}  ")).unwrap_or_default())));
        true
    });
    card.append(&area);
    (card, area)
}

/// The graph's slices for the chosen span: (slices, their start, seconds each).
fn speed_slices() -> (Vec<Option<(f64, f64)>>, i64, f64) {
    let now = glib::real_time() / 1_000_000;
    let (from, to, n) = match prefs::get().home_span.as_str() {
        "5min" => (now - 300, now, 30),
        "today" => {
            let midnight = glib::DateTime::now_local()
                .map(|d| d.to_unix() - (d.hour() * 3600 + d.minute() * 60 + d.second()) as i64)
                .unwrap_or(now - 86_400);
            (midnight, midnight + 86_400, 24)
        }
        _ => (now - 3600, now, 30),
    };
    (live::speed_bars(from, to, n), from, (to - from) as f64 / n as f64)
}

fn draw_speed(_area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: f64, h: f64) {
    let (bars, _, _) = speed_slices();
    if bars.is_empty() {
        return;
    }
    let max = graph::nice_ceiling(bars.iter().flatten().map(|(d, u)| d.max(*u)).fold(0.0, f64::max).max(1024.0));
    let gap = 4.0;
    let bw = ((w - gap * (bars.len() - 1) as f64) / bars.len() as f64).max(1.0);
    let down = Rgb::hex(&theme::app_accent("torrents"));
    let up = Rgb::hex(&theme::look().success);
    let border = Rgb::hex(&theme::palette().border);
    let bar = |x: f64, value: f64, c: Rgb, a: f64| {
        let bh = (value / max).clamp(0.0, 1.0) * h;
        if bh < 0.5 {
            return;
        }
        let r = 3.0f64.min(bw / 2.0).min(bh);
        let y = h - bh;
        cr.new_sub_path();
        cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
        cr.arc(x + bw - r, y + r, r, 1.5 * std::f64::consts::PI, 2.0 * std::f64::consts::PI);
        cr.line_to(x + bw, h);
        cr.line_to(x, h);
        cr.close_path();
        cr.set_source_rgba(c.0, c.1, c.2, a);
        let _ = cr.fill();
    };
    for (i, slice) in bars.iter().enumerate() {
        let x = i as f64 * (bw + gap);
        match slice {
            Some((dl, ul)) => {
                cr.rectangle(x, h - 2.0, bw, 2.0);
                cr.set_source_rgba(border.0, border.1, border.2, 0.5);
                let _ = cr.fill();
                bar(x, *dl, down, 1.0);
                bar(x, *ul, up, 0.95);
            }
            None => {
                cr.rectangle(x, h - 2.0, bw, 2.0);
                cr.set_source_rgba(border.0, border.1, border.2, 0.35);
                let _ = cr.fill();
            }
        }
    }
}

pub fn build(page: &Page) {
    let content = widgets::vbox(0);
    content.add_css_class("home");

    let hello = widgets::hbox(12);
    hello.add_css_class("home-hello");
    let words = widgets::vbox(2);
    words.set_hexpand(true);
    let title = widgets::label("", "home-title");
    title.set_ellipsize(pango::EllipsizeMode::End);
    let line = widgets::label("", "dim");
    line.set_wrap(true);
    words.append(&title);
    words.append(&line);
    hello.append(&words);
    let pause_all = widgets::labeled_button("media-playback-pause-symbolic", "Pause all");
    pause_all.set_valign(gtk::Align::Center);
    pause_all.connect_clicked(|_| pause_or_resume_all());
    hello.append(&pause_all);
    content.append(&hello);

    let flow = gtk::FlowBox::new();
    flow.add_css_class("stat-grid");
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(2);
    flow.set_max_children_per_line(4);
    flow.set_row_spacing(10);
    flow.set_column_spacing(10);
    let cards = [
        stat_card("go-down-symbolic", "Down now"),
        stat_card("go-up-symbolic", "Up now"),
        stat_card("send-to-symbolic", "Shared in all"),
        stat_card("drive-harddisk-symbolic", "Free space"),
    ];
    let stats = cards.map(|(card, stat)| {
        card.set_size_request(150, -1);
        flow.append(&card);
        stat
    });
    let mut child = flow.first_child();
    while let Some(c) = child {
        c.set_focusable(false);
        child = c.next_sibling();
    }
    content.append(&flow);

    let (speed, graph) = speed_card();
    content.append(&speed);

    let two = widgets::hbox(18);
    two.set_homogeneous(true);
    two.add_css_class("home-two");
    let left = widgets::vbox(0);
    left.append(&section_head("Downloading now", Some("downloading")));
    let downloading = widgets::vbox(8);
    left.append(&downloading);
    two.append(&left);
    let right = widgets::vbox(0);
    right.append(&section_head("Connection", None));
    let conn_card = widgets::vbox(0);
    conn_card.add_css_class("card");
    conn_card.add_css_class("conn-card");
    let conn = [
        conn_row(&conn_card, "security-high-symbolic", "connection"),
        conn_row(&conn_card, "network-wired-symbolic", "connection"),
        conn_row(&conn_card, "network-workgroup-symbolic", "bittorrent"),
        conn_row(&conn_card, "torrents-turtle-symbolic", "speed"),
    ];
    right.append(&conn_card);
    two.append(&right);
    content.append(&two);

    content.append(&section_head("Finished recently", Some("completed")));
    let finished = widgets::vbox(0);
    content.append(&finished);

    let clamp = nyx_ui::clamp::Clamp::new(&content, 940);
    clamp.set_hexpand(true);
    page.body.append(&clamp);

    let h = Home {
        title,
        line,
        pause_all,
        stats,
        graph,
        two,
        downloading,
        downloading_ids: vec!["-".into()],
        minis: Vec::new(),
        conn,
        finished,
        finished_ids: vec!["-".into()],
        finished_rows: Vec::new(),
    };
    HOME.with(|s| *s.borrow_mut() = Some(Rc::new(RefCell::new(h))));
    live::on_tick(&clamp, refresh);
    if live::latest().is_none() {
        refresh(&Update::default());
    }
}

/// The two halves (downloading, connection) stack in a narrow window.
pub fn set_narrow(narrow: bool) {
    if let Some(h) = home() {
        let two = h.borrow().two.clone();
        two.set_orientation(if narrow { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        two.set_spacing(if narrow { 6 } else { 18 });
        two.set_homogeneous(!narrow);
    }
}

fn pause_or_resume_all() {
    let Some(u) = live::latest() else { return };
    let running: Vec<String> = u.torrents.iter().filter(|s| !model::stopped(s)).map(|s| s.id.clone()).collect();
    if running.is_empty() {
        let ids = u.torrents.iter().map(|s| s.id.clone()).collect();
        live::send(Command::Resume { ids, force: false });
    } else {
        live::send(Command::Pause(running));
    }
}

// ---------- Refresh ----------

fn refresh(u: &Update) {
    let Some(h) = home() else { return };
    let p = prefs::get();
    let torrents = &u.torrents;
    let matching = |f: Filter| -> Vec<&Status> { torrents.iter().filter(|s| f.matches(s, &store::meta(&s.id))).collect() };
    let mut downloading = matching(Filter::Downloading);
    downloading.sort_by_key(|s| if s.queue_position >= 0 { s.queue_position as i64 } else { i64::MAX });
    let seeding = matching(Filter::Seeding);
    let errored = matching(Filter::Errored);
    let paused = matching(Filter::Paused).len();

    {
        let hb = h.borrow();
        hb.title.set_text(&if torrents.is_empty() {
            "No torrents yet".to_string()
        } else if downloading.is_empty() && seeding.is_empty() && paused == torrents.len() {
            "Everything is paused".to_string()
        } else if downloading.is_empty() && seeding.is_empty() {
            "Nothing running".to_string()
        } else {
            format!("Downloading {} · seeding {}", downloading.len(), seeding.len())
        });
        let next = downloading.iter().filter_map(|s| model::eta(s).map(|e| (e, *s))).min_by_key(|(e, _)| *e);
        let line = if let Some(s) = errored.first() {
            let n = errored.len();
            format!(
                "{n} torrent{} need{} you: {}: {}.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
                short_name(&s.name),
                s.error.trim_end_matches('.')
            )
        } else if u.stats.listen_port > 0 && !u.stats.has_incoming {
            "Not listening for peers: check the port and network in Connection.".to_string()
        } else if torrents.is_empty() {
            "Add a .torrent file or a magnet link, search the sites, or drop one on this window.".to_string()
        } else if paused == torrents.len() {
            format!(
                "{} paused. Resume all to carry on.",
                if paused == 1 { "1 torrent is".to_string() } else { format!("{paused} torrents are") }
            )
        } else if let Some((eta, s)) = next {
            format!("Everything's fine. {} finishes in about {}.", short_name(&s.name), about(eta))
        } else {
            "Everything's fine.".to_string()
        };
        hb.line.set_text(&line);
        hb.pause_all.set_visible(!torrents.is_empty());
        let any_running = torrents.iter().any(|s| !model::stopped(s));
        if let Some(label) = hb.pause_all.child().and_then(|c| c.last_child()).and_downcast::<gtk::Label>() {
            label.set_text(if any_running { "Pause all" } else { "Resume all" });
        }
        if let Some(icon) = hb.pause_all.child().and_then(|c| c.first_child()).and_downcast::<gtk::Image>() {
            icon.set_icon_name(Some(if any_running { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" }));
        }

        let alt = live::alt_active();
        let (dl_limit, ul_limit) = if alt { (p.alt_dl_limit, p.alt_ul_limit) } else { (p.dl_limit, p.ul_limit) };
        let slow = if alt { " · slow mode" } else { "" };
        let [down, up, shared, free] = &hb.stats;
        down.value.set_text(&fmt::net_rate(u.stats.dl_rate as f64));
        down.sub.set_text(&format!("{}{slow}", limit_text(dl_limit)));
        up.value.set_text(&fmt::net_rate(u.stats.ul_rate as f64));
        up.sub.set_text(&format!("{}{slow}", limit_text(ul_limit)));
        let uploaded: i64 = torrents.iter().map(|s| s.uploaded).sum();
        let downloaded: i64 = torrents.iter().map(|s| s.downloaded.max(if s.downloaded == 0 { s.total_done } else { 0 })).sum();
        shared.value.set_text(&fmt::bytes(uploaded as f64));
        shared.sub.set_text(&if downloaded > 0 {
            format!("Ratio {:.2} overall", uploaded as f64 / downloaded as f64)
        } else {
            "Nothing downloaded yet".into()
        });
        free.value.set_text(&free_space(&p.save_path).map(|b| fmt::bytes(b as f64)).unwrap_or_else(|| "–".into()));
        free.sub.set_text(&paths::pretty(&paths::expand(&p.save_path)));

        let [vpn, port, dht, slow_row] = &hb.conn;
        let listening = u.stats.has_incoming;
        let iface = p.interface.trim();
        if iface.is_empty() {
            vpn.key.set_text("Not bound to a network");
            vpn.value.set_text("Any network");
            set_tone(&vpn.value, None);
        } else {
            vpn.key.set_text(&format!("Bound to {iface}{}", if looks_like_vpn(iface) { " (your VPN)" } else { "" }));
            vpn.value.set_text(if listening { "Up" } else { "Down" });
            set_tone(&vpn.value, Some(if listening { "ok" } else { "bad" }));
        }
        port.key.set_text(&if u.stats.listen_port > 0 {
            format!("Port {}", u.stats.listen_port)
        } else {
            format!("Port {}", p.port)
        });
        port.value.set_text(if listening { "Listening" } else { "Closed" });
        set_tone(&port.value, Some(if listening { "ok" } else { "bad" }));
        dht.key.set_text("Peers found without trackers");
        dht.value.set_text(&if p.dht { format!("{} nodes", u.stats.dht_nodes) } else { "Off".into() });
        slow_row.key.set_text("Slow mode");
        let days = match p.schedule_days.as_str() {
            "weekdays" => "Weekdays",
            "weekends" => "Weekends",
            _ => "Every day",
        };
        slow_row.value.set_text(&if p.schedule {
            format!("{days} {}–{}", p.schedule_from, p.schedule_to)
        } else if alt {
            "On".into()
        } else {
            "Off".into()
        });
        set_tone(&slow_row.value, alt.then_some("accent"));
        hb.graph.queue_draw();
    }

    let shown: Vec<&Status> = downloading.iter().take(DOWNLOADING_SHOWN).copied().collect();
    let ids: Vec<String> = shown.iter().map(|s| s.id.clone()).collect();
    if h.borrow().downloading_ids != ids {
        rebuild_downloading(&h, &shown);
        h.borrow_mut().downloading_ids = ids;
    }
    for (mini, s) in h.borrow().minis.iter().zip(&shown) {
        mini.bar.set_fraction(s.progress.clamp(0.0, 1.0));
        let mut parts = vec![if s.dl_rate > 0 { fmt::net_rate(s.dl_rate as f64) } else { model::kind(s).label().to_string() }];
        if let Some(eta) = model::eta(s) {
            parts.push(format!("{} left", fmt::duration(eta as f64)));
        }
        parts.push(format!("{} peers", s.peers));
        mini.line.set_text(&parts.join(" · "));
        let paused = model::stopped(s);
        mini.pause.set_icon_name(if paused { "media-playback-start-symbolic" } else { "media-playback-pause-symbolic" });
        mini.pause.set_tooltip_text(Some(if paused { "Resume" } else { "Pause" }));
    }

    let mut done: Vec<&Status> = torrents.iter().filter(|s| model::done(s)).collect();
    done.sort_by_key(|s| std::cmp::Reverse(if s.completed > 0 { s.completed } else { s.added }));
    done.truncate(FINISHED_SHOWN);
    let ids: Vec<String> = done.iter().map(|s| s.id.clone()).collect();
    if h.borrow().finished_ids != ids {
        rebuild_finished(&h, &done);
        h.borrow_mut().finished_ids = ids;
    }
    for (row, s) in h.borrow().finished_rows.iter().zip(&done) {
        row.meta.set_text(&format!(
            "{} · ratio {}",
            fmt::bytes(s.wanted as f64),
            crate::sections::transfers::ratio_text(model::ratio(s))
        ));
        row.when.set_text(&when(if s.completed > 0 { s.completed } else { s.added }));
    }
}

fn rebuild_downloading(h: &Rc<RefCell<Home>>, shown: &[&Status]) {
    let list = h.borrow().downloading.clone();
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    let mut minis = Vec::new();
    if shown.is_empty() {
        list.append(&empty_card("Nothing is downloading."));
    }
    for s in shown {
        let card = widgets::hbox(12);
        card.add_css_class("card");
        card.add_css_class("mini-card");
        card.append(&widgets::file_tile(&s.name));
        let text = widgets::vbox(5);
        text.set_hexpand(true);
        let name = widgets::label(&s.name, "mini-name");
        name.set_ellipsize(pango::EllipsizeMode::Middle);
        name.set_tooltip_text(Some(&s.name));
        text.append(&name);
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("cell-progress");
        text.append(&bar);
        let line = widgets::label("", "mini-line");
        line.add_css_class("tnum");
        text.append(&line);
        card.append(&text);
        let pause = widgets::icon_button("media-playback-pause-symbolic", "Pause");
        let id = s.id.clone();
        pause.connect_clicked(move |_| actions::toggle_pause(std::slice::from_ref(&id)));
        card.append(&pause);
        list.append(&card);
        minis.push(Mini { bar, line, pause });
    }
    h.borrow_mut().minis = minis;
}

fn rebuild_finished(h: &Rc<RefCell<Home>>, done: &[&Status]) {
    let list = h.borrow().finished.clone();
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    list.remove_css_class("card");
    let mut rows = Vec::new();
    if done.is_empty() {
        list.append(&empty_card("Nothing has finished yet."));
    } else {
        list.add_css_class("card");
        list.add_css_class("finished-card");
    }
    for (i, s) in done.iter().enumerate() {
        if i > 0 {
            let line = gtk::Separator::new(gtk::Orientation::Horizontal);
            line.add_css_class("row-line");
            line.add_css_class("tile-inset");
            list.append(&line);
        }
        let row = widgets::hbox(12);
        row.add_css_class("finished-row");
        row.append(&widgets::file_tile(&s.name));
        let name = widgets::label(&s.name, "");
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::Middle);
        name.set_tooltip_text(Some(&s.name));
        row.append(&name);
        let meta = widgets::label("", "finished-meta");
        meta.add_css_class("tnum");
        meta.set_width_chars(18);
        meta.set_xalign(1.0);
        row.append(&meta);
        let when = widgets::label("", "finished-meta");
        when.set_width_chars(12);
        row.append(&when);
        let open = gtk::Button::with_label("Open");
        open.add_css_class("small-button");
        open.set_valign(gtk::Align::Center);
        let id = s.id.clone();
        open.connect_clicked(move |_| actions::open_content(&id));
        row.append(&open);
        let folder = widgets::icon_button("folder-open-symbolic", "Show in folder");
        let id = s.id.clone();
        folder.connect_clicked(move |_| actions::open_folder(&id));
        row.append(&folder);
        list.append(&row);
        rows.push(Finished { meta, when });
    }
    h.borrow_mut().finished_rows = rows;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_names_and_times_plainly() {
        assert_eq!(short_name("debian-13.2.0-amd64-DVD-1.iso"), "Debian");
        assert_eq!(short_name("Nosferatu (1922) restored 1080p"), "Nosferatu");
        assert_eq!(short_name("ab-c"), "Ab-c");
        assert_eq!(about(30), "a minute");
        assert_eq!(about(240), "4 minutes");
        assert_eq!(about(7200), "2 hours");
        assert!(looks_like_vpn("wg0"));
        assert!(!looks_like_vpn("enp3s0"));
    }
}
