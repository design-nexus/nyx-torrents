//! The torrent list. One stable object per torrent lives in a `gio::ListStore`;
//! every update we refresh their data in place, work out visibility and order
//! ourselves, and only poke GTK's filter/sorter when the order really changed,
//! keeping the selection and scroll position. The details pane sits below.
//! Home's "See all" links and the sidebar's filters lead here.

use super::details;
use crate::engine::{Command, Queue, Update};
use crate::model::{self, Filter, Row, TorrentObject};
use crate::widgets::{self, Page};
use crate::{actions, fmt, live, prefs, store};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Col {
    Queue,
    Name,
    Size,
    Progress,
    Status,
    Down,
    Up,
    Eta,
    Seeds,
    Peers,
    Ratio,
    Category,
    Tags,
    Added,
    Availability,
}

/// (column, id, title, width; 0 expands)
const COLUMNS: &[(Col, &str, &str, i32)] = &[
    (Col::Queue, "queue", "#", 44),
    (Col::Name, "name", "Name", 0),
    (Col::Size, "size", "Size", 92),
    (Col::Progress, "progress", "Progress", 124),
    (Col::Status, "status", "Status", 116),
    (Col::Down, "down", "Down", 104),
    (Col::Up, "up", "Up", 104),
    (Col::Eta, "eta", "ETA", 72),
    (Col::Seeds, "seeds", "Seeds", 84),
    (Col::Peers, "peers", "Peers", 84),
    (Col::Ratio, "ratio", "Ratio", 60),
    (Col::Category, "category", "Category", 110),
    (Col::Tags, "tags", "Tags", 110),
    (Col::Added, "added", "Added", 128),
    (Col::Availability, "availability", "Availability", 96),
];

/// As the table narrows, columns go in this order so Name keeps room to breathe.
const DROP_ORDER: &[Col] = &[
    Col::Availability,
    Col::Added,
    Col::Tags,
    Col::Category,
    Col::Ratio,
    Col::Peers,
    Col::Seeds,
    Col::Queue,
    Col::Eta,
    Col::Up,
    Col::Status,
    Col::Size,
];

/// The least room the Name column gets before other columns start to hide.
const NAME_MIN: i32 = 260;

/// Room the table itself takes around its columns (row margins, borders, padding).
const ROW_INSET: i32 = 32;

struct State {
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    sorter: gtk::CustomSorter,
    selection: gtk::MultiSelection,
    view: gtk::ColumnView,
    scroll: gtk::ScrolledWindow,
    columns: Vec<(Col, gtk::ColumnViewColumn)>,
    by_id: HashMap<String, TorrentObject>,
    bound: HashMap<usize, (TorrentObject, Col, gtk::Widget)>,
    current: Filter,
    query: String,
    order: Vec<String>,
    search: gtk::SearchEntry,
    empty: gtk::Stack,
    empty_label: gtk::Label,
    buttons: Vec<gtk::Widget>,
    /// The table width the columns were last fitted to.
    fitted: i32,
    queue_box: gtk::Box,
    selected_id: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
}

fn state() -> Option<Rc<RefCell<State>>> {
    STATE.with(|s| s.borrow().clone())
}

fn key(w: &gtk::Widget) -> usize {
    w.as_ptr() as usize
}

// ---------- Cells ----------

fn setup_cell(col: Col) -> gtk::Widget {
    match col {
        Col::Name => {
            let b = widgets::hbox(8);
            let icon = gtk::Image::new();
            icon.add_css_class("cell-icon");
            let l = gtk::Label::new(None);
            l.set_xalign(0.0);
            l.set_ellipsize(pango::EllipsizeMode::Middle);
            l.set_hexpand(true);
            b.append(&icon);
            b.append(&l);
            b.upcast()
        }
        Col::Progress => {
            let b = widgets::hbox(8);
            let bar = gtk::ProgressBar::new();
            bar.add_css_class("cell-progress");
            bar.set_hexpand(true);
            bar.set_valign(gtk::Align::Center);
            let l = gtk::Label::new(None);
            l.add_css_class("cell-num");
            l.set_xalign(1.0);
            l.set_width_chars(6);
            b.append(&bar);
            b.append(&l);
            b.upcast()
        }
        _ => {
            let l = gtk::Label::new(None);
            let numeric = matches!(
                col,
                Col::Queue
                    | Col::Size
                    | Col::Down
                    | Col::Up
                    | Col::Eta
                    | Col::Seeds
                    | Col::Peers
                    | Col::Ratio
                    | Col::Added
                    | Col::Availability
            );
            if numeric {
                l.add_css_class("cell-num");
                l.set_xalign(1.0);
            } else {
                l.set_xalign(0.0);
            }
            l.set_ellipsize(pango::EllipsizeMode::End);
            l.upcast()
        }
    }
}

fn set_text(w: &gtk::Widget, text: &str) {
    if let Some(l) = w.downcast_ref::<gtk::Label>()
        && l.text() != text
    {
        l.set_text(text);
    }
}

pub fn peers_text(connected: i32, total: i32) -> String {
    let short = |n: i32| {
        if n >= 10_000 {
            format!("{}k", n / 1000)
        } else if n >= 1000 {
            format!("{:.1}k", n as f64 / 1000.0)
        } else {
            n.to_string()
        }
    };
    if total > 0 { format!("{connected} ({})", short(total)) } else { connected.to_string() }
}

pub fn ratio_text(r: f64) -> String {
    if r.is_infinite() { "∞".into() } else { format!("{r:.2}") }
}

pub fn date_text(ts: i64) -> String {
    if ts <= 0 {
        return "–".into();
    }
    glib::DateTime::from_unix_local(ts)
        .ok()
        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn render(col: Col, row: &Row, w: &gtk::Widget) {
    let s = &row.s;
    match col {
        Col::Name => {
            let (Some(icon), Some(label)) =
                (w.first_child().and_downcast::<gtk::Image>(), w.last_child().and_downcast::<gtk::Label>())
            else {
                return;
            };
            let kind = model::kind(s);
            icon.set_icon_name(Some(kind.icon()));
            for (k, class) in [(model::Kind::Error, "cell-error"), (model::Kind::Downloading, "cell-hot")] {
                if kind == k {
                    icon.add_css_class(class);
                } else {
                    icon.remove_css_class(class);
                }
            }
            if label.text() != s.name.as_str() {
                label.set_text(&s.name);
                label.set_tooltip_text(Some(&s.name));
            }
        }
        Col::Progress => {
            let (Some(bar), Some(label)) =
                (w.first_child().and_downcast::<gtk::ProgressBar>(), w.last_child().and_downcast::<gtk::Label>())
            else {
                return;
            };
            bar.set_fraction(s.progress.clamp(0.0, 1.0));
            if model::done(s) {
                bar.add_css_class("done");
            } else {
                bar.remove_css_class("done");
            }
            let pct = s.progress * 100.0;
            let text = if pct >= 100.0 { "100%".into() } else { format!("{pct:.1}%") };
            if label.text() != text {
                label.set_text(&text);
            }
        }
        Col::Queue => set_text(w, &if s.queue_position >= 0 { (s.queue_position + 1).to_string() } else { "*".into() }),
        Col::Size => set_text(w, &if s.has_metadata { fmt::bytes(s.wanted as f64) } else { "–".into() }),
        Col::Status => {
            set_text(w, &model::status_text(s));
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                l.set_tooltip_text((!s.error.is_empty()).then_some(s.error.as_str()));
                if s.error.is_empty() {
                    l.remove_css_class("danger-text");
                } else {
                    l.add_css_class("danger-text");
                }
            }
        }
        Col::Down => set_text(w, &if s.dl_rate > 0 { fmt::net_rate(s.dl_rate as f64) } else { String::new() }),
        Col::Up => set_text(w, &if s.ul_rate > 0 { fmt::net_rate(s.ul_rate as f64) } else { String::new() }),
        Col::Eta => set_text(w, &model::eta(s).map(|e| fmt::duration(e as f64)).unwrap_or_else(|| "∞".into())),
        Col::Seeds => set_text(w, &peers_text(s.seeds, s.seeds_total)),
        Col::Peers => set_text(w, &peers_text(s.peers, s.peers_total)),
        Col::Ratio => set_text(w, &ratio_text(model::ratio(s))),
        Col::Category => set_text(w, &row.meta.category),
        Col::Tags => set_text(w, &row.meta.tags.join(", ")),
        Col::Added => set_text(w, &date_text(s.added)),
        Col::Availability => {
            set_text(w, &if s.availability >= 0.0 && !model::done(s) { format!("{:.2}", s.availability) } else { "–".into() })
        }
    }
    if let Some(cell) = w.parent() {
        if model::stopped(s) {
            cell.add_css_class("row-paused");
        } else {
            cell.remove_css_class("row-paused");
        }
    }
}

fn make_column(col: Col, title: &str, width: i32) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        item.set_child(Some(&setup_cell(col)));
    });
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(obj), Some(w)) = (item.item().and_downcast::<TorrentObject>(), item.child()) else { return };
        render(col, &obj.row(), &w);
        if let Some(st) = state() {
            st.borrow_mut().bound.insert(key(&w), (obj, col, w));
        }
    });
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(w), Some(st)) = (item.child(), state()) {
            st.borrow_mut().bound.remove(&key(&w));
        }
    });
    let c = gtk::ColumnViewColumn::new(Some(title), Some(factory));
    // A sorter that never reorders; it only gives the header its sort indicator.
    // The real order is worked out in `relayout`.
    c.set_sorter(Some(&gtk::CustomSorter::new(|_, _| gtk::Ordering::Equal)));
    c.set_resizable(true);
    if width > 0 {
        c.set_fixed_width(width);
    } else {
        c.set_expand(true);
    }
    c
}

// ---------- Order and visibility ----------

fn sort_state(st: &State) -> (Col, bool) {
    let Some(cvs) = st.view.sorter().and_downcast::<gtk::ColumnViewSorter>() else { return (Col::Queue, false) };
    let Some(primary) = cvs.primary_sort_column() else { return (Col::Queue, false) };
    let col = st.columns.iter().find(|(_, c)| c == &primary).map(|(c, _)| *c).unwrap_or(Col::Queue);
    (col, cvs.primary_sort_order() == gtk::SortType::Descending)
}

fn compare(col: Col, a: &Row, b: &Row) -> Ordering {
    let (x, y) = (&a.s, &b.s);
    let f = |v: f64, w: f64| v.partial_cmp(&w).unwrap_or(Ordering::Equal);
    match col {
        // Queued torrents first in queue order, then the rest (seeding, finished).
        Col::Queue => {
            let q = |s: &crate::engine::Status| if s.queue_position >= 0 { s.queue_position as i64 } else { i64::MAX };
            q(x).cmp(&q(y))
        }
        Col::Name => x.name.to_lowercase().cmp(&y.name.to_lowercase()),
        Col::Size => x.wanted.cmp(&y.wanted),
        Col::Progress => f(x.progress, y.progress),
        Col::Status => model::status_text(x).cmp(&model::status_text(y)),
        Col::Down => x.dl_rate.cmp(&y.dl_rate),
        Col::Up => x.ul_rate.cmp(&y.ul_rate),
        Col::Eta => model::eta(x).unwrap_or(i64::MAX).cmp(&model::eta(y).unwrap_or(i64::MAX)),
        Col::Seeds => (x.seeds, x.seeds_total).cmp(&(y.seeds, y.seeds_total)),
        Col::Peers => (x.peers, x.peers_total).cmp(&(y.peers, y.peers_total)),
        Col::Ratio => f(model::ratio(x), model::ratio(y)),
        Col::Category => a.meta.category.to_lowercase().cmp(&b.meta.category.to_lowercase()),
        Col::Tags => a.meta.tags.join(",").cmp(&b.meta.tags.join(",")),
        Col::Added => x.added.cmp(&y.added),
        Col::Availability => f(x.availability, y.availability),
    }
}

fn matches_query(row: &Row, q: &str) -> bool {
    q.is_empty()
        || q.split_whitespace().all(|t| {
            row.s.name.to_lowercase().contains(t) || row.meta.category.to_lowercase().contains(t) || row.s.id.starts_with(t)
        })
}

fn selected_ids(st: &State) -> Vec<String> {
    let bits = st.selection.selection();
    let mut out = Vec::new();
    let n = bits.size();
    for i in 0..n {
        let pos = bits.nth(i as u32);
        if let Some(o) = st.selection.item(pos).and_downcast::<TorrentObject>() {
            out.push(o.row().s.id.clone());
        }
    }
    out
}

fn restore_selection(selection: &gtk::MultiSelection, ids: &HashSet<String>) {
    let set = gtk::Bitset::new_empty();
    for i in 0..selection.n_items() {
        if let Some(o) = selection.item(i).and_downcast::<TorrentObject>()
            && ids.contains(&o.row().s.id)
        {
            set.add(i);
        }
    }
    let mask = gtk::Bitset::new_range(0, selection.n_items());
    selection.set_selection(&set, &mask);
}

/// Work out which torrents show and in what order; touch GTK only if it changed.
fn relayout(st_rc: &Rc<RefCell<State>>) {
    let (objects, (col, desc), filter, query) = {
        let st = st_rc.borrow();
        let objs: Vec<TorrentObject> = st.by_id.values().cloned().collect();
        (objs, sort_state(&st), st.current.clone(), st.query.to_lowercase())
    };
    let mut visible: Vec<TorrentObject> = objects
        .iter()
        .filter(|o| {
            let r = o.row();
            filter.matches(&r.s, &r.meta) && matches_query(&r, &query)
        })
        .cloned()
        .collect();
    visible.sort_by(|a, b| {
        let (ra, rb) = (a.row(), b.row());
        let o = compare(col, &ra, &rb);
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| ra.s.name.to_lowercase().cmp(&rb.s.name.to_lowercase())).then_with(|| ra.s.id.cmp(&rb.s.id))
    });
    let order: Vec<String> = visible.iter().map(|o| o.row().s.id.clone()).collect();
    let rank: HashMap<&str, u32> = order.iter().enumerate().map(|(i, id)| (id.as_str(), i as u32)).collect();
    for o in &objects {
        let mut r = o.row_mut();
        let pos = rank.get(r.s.id.as_str()).copied();
        r.visible = pos.is_some();
        r.order = pos.unwrap_or(u32::MAX);
    }

    let (changed, filter_model, sorter, scroll, selection, ids) = {
        let st = st_rc.borrow();
        let changed = st.order != order;
        let ids: HashSet<String> = if changed { selected_ids(&st).into_iter().collect() } else { HashSet::new() };
        (changed, st.filter.clone(), st.sorter.clone(), st.scroll.clone(), st.selection.clone(), ids)
    };
    if changed {
        let value = scroll.vadjustment().value();
        // GTK re-binds rows synchronously here, so no state borrow may be held.
        filter_model.changed(gtk::FilterChange::Different);
        sorter.changed(gtk::SorterChange::Different);
        restore_selection(&selection, &ids);
        let mut st = st_rc.borrow_mut();
        st.order = order;
        let empty = st.order.is_empty();
        let none_at_all = st.by_id.is_empty();
        st.empty.set_visible_child_name(if empty { "empty" } else { "list" });
        st.empty_label.set_text(if none_at_all {
            "No torrents yet. Add a .torrent file or a magnet link, or drop one on this window."
        } else if !st.query.is_empty() {
            "No torrents match your filter."
        } else {
            "Nothing here right now."
        });
        drop(st);
        // ListView follows its anchor row after a re-sort; keep the view still instead.
        glib::idle_add_local_once(move || scroll.vadjustment().set_value(value));
    }
    sync_details(st_rc);
}

fn refresh_cells(st_rc: &Rc<RefCell<State>>) {
    let bound: Vec<(TorrentObject, Col, gtk::Widget)> = st_rc.borrow().bound.values().cloned().collect();
    for (obj, col, w) in bound {
        render(col, &obj.row(), &w);
    }
}

/// Point the details pane at the first selected torrent, or at nothing. Also runs
/// after the list changes: a removed or filtered-out torrent leaves the selection
/// without GTK reporting a selection change.
fn sync_details(st_rc: &Rc<RefCell<State>>) {
    let sel = st_rc.borrow().selection.clone();
    let bits = sel.selection();
    let first = (bits.size() > 0)
        .then(|| sel.item(bits.nth(0)).and_downcast::<TorrentObject>())
        .flatten()
        .map(|o| o.row().s.id.clone());
    if st_rc.borrow().selected_id != first {
        st_rc.borrow_mut().selected_id = first.clone();
        details::show(first);
    }
}

fn update(u: &Update) {
    let Some(st_rc) = state() else { return };
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let store = st_rc.borrow().store.clone();
    {
        let mut st = st_rc.borrow_mut();
        let mut seen = HashSet::new();
        for t in &u.torrents {
            seen.insert(t.id.clone());
            let meta = store::meta(&t.id);
            if let Some(o) = st.by_id.get(&t.id) {
                let mut r = o.row_mut();
                r.s = t.clone();
                r.meta = meta;
            } else {
                let o = TorrentObject::new(Row { s: t.clone(), meta, visible: false, order: u32::MAX });
                st.by_id.insert(t.id.clone(), o.clone());
                added.push(o);
            }
        }
        let gone: Vec<String> = st.by_id.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for id in gone {
            if let Some(o) = st.by_id.remove(&id) {
                removed.push(o);
            }
        }
    }
    // Store changes re-bind rows synchronously: do them with no borrow held.
    for o in removed {
        if let Some(pos) = store.find(&o) {
            store.remove(pos);
        }
    }
    if !added.is_empty() {
        store.extend_from_slice(&added);
    }
    relayout(&st_rc);
    refresh_cells(&st_rc);
    update_buttons(&st_rc);
}

fn update_buttons(st_rc: &Rc<RefCell<State>>) {
    let st = st_rc.borrow();
    let any = st.selection.selection().size() > 0;
    for b in &st.buttons {
        b.set_sensitive(any);
    }
}

/// Re-read categories and tags (after a change from a menu), without waiting for the tick.
pub fn refresh_meta() {
    let Some(st_rc) = state() else { return };
    for o in st_rc.borrow().by_id.values() {
        let id = o.row().s.id.clone();
        o.row_mut().meta = store::meta(&id);
    }
    st_rc.borrow_mut().order.clear();
    relayout(&st_rc);
    refresh_cells(&st_rc);
}

// ---------- Public API ----------

/// A filter's name for the top bar.
pub fn filter_title(f: &Filter) -> String {
    match f {
        Filter::All => "All torrents".to_string(),
        Filter::Category(Some(c)) => format!("Category: {c}"),
        Filter::Tag(Some(t)) => format!("Tag: {t}"),
        other => other.title(),
    }
}

pub fn set_filter(f: Filter) {
    let Some(st_rc) = state() else { return };
    {
        let mut st = st_rc.borrow_mut();
        st.current = f;
        st.order.clear();
    }
    relayout(&st_rc);
}

pub fn focus_search() {
    if let Some(st) = state() {
        st.borrow().search.grab_focus();
    }
}

/// Ids of the selected torrents, in list order.
pub fn selection() -> Vec<String> {
    state().map(|st| selected_ids(&st.borrow())).unwrap_or_default()
}

/// A torrent's row, from the list or (before the list is first shown) the latest update.
pub fn row(id: &str) -> Option<Row> {
    if let Some(st) = state() {
        return st.borrow().by_id.get(id).map(|o| o.row().clone());
    }
    let u = live::latest()?;
    let s = u.torrents.iter().find(|t| t.id == id)?.clone();
    Some(Row { s, meta: store::meta(id), visible: false, order: u32::MAX })
}

pub fn set_narrow(_narrow: bool) {
    if let Some(st) = state() {
        st.borrow_mut().fitted = 0;
    }
}

/// Show the columns the user wants, dropping low-priority ones that don't fit.
/// Takes no borrow of the state: hiding a column unbinds cells, which borrows it.
fn apply_column_visibility(columns: &[(Col, gtk::ColumnViewColumn)], width: i32) {
    let hidden = prefs::get().hidden_columns;
    let wanted: Vec<Col> = columns
        .iter()
        .filter(|(col, c)| {
            let id = c.id().map(|s| s.to_string()).unwrap_or_default();
            *col == Col::Name || !hidden.contains(&id)
        })
        .map(|(col, _)| *col)
        .collect();
    let fixed =
        |cols: &[Col]| -> i32 { cols.iter().map(|c| COLUMNS.iter().find(|x| x.0 == *c).map(|x| x.3).unwrap_or(0)).sum::<i32>() };
    let mut show = wanted.clone();
    if width > 0 {
        for drop in DROP_ORDER {
            if fixed(&show) + NAME_MIN + ROW_INSET <= width {
                break;
            }
            show.retain(|c| c != drop);
        }
    }
    for (col, c) in columns {
        c.set_visible(show.contains(col));
    }
}

fn fit_columns() {
    let Some(st_rc) = state() else { return };
    // The table card's width, not the view's: the view never gets narrower than its
    // columns. (The card is allocated even while the empty state shows.)
    let width = st_rc.borrow().empty.width();
    if width <= 0 || width == st_rc.borrow().fitted {
        return;
    }
    st_rc.borrow_mut().fitted = width;
    let (columns, queue_box) = {
        let st = st_rc.borrow();
        (st.columns.clone(), st.queue_box.clone())
    };
    queue_box.set_visible(width >= 720);
    apply_column_visibility(&columns, width);
}

// ---------- Build ----------

fn tool_button(icon: &str, tooltip: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("tool-button");
    b.connect_clicked(move |_| f());
    b
}

pub fn build(page: &Page) {
    let p = prefs::get();

    // ----- Toolbar -----
    let toolbar = widgets::hbox(6);
    toolbar.add_css_class("toolbar");
    let magnet = tool_button("torrents-magnet-symbolic", "Add a magnet link (Ctrl+U)", || actions::add_magnet_dialog(""));
    toolbar.append(&magnet);
    toolbar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let resume = tool_button("media-playback-start-symbolic", "Resume (Space)", || {
        live::send(Command::Resume { ids: selection(), force: false })
    });
    let pause = tool_button("media-playback-pause-symbolic", "Pause (Space)", || live::send(Command::Pause(selection())));
    let remove = tool_button("user-trash-symbolic", "Remove (Delete)", || actions::remove_dialog(selection()));
    let up = tool_button("go-up-symbolic", "Move up in the queue", || live::send(Command::Queue(selection(), Queue::Up)));
    let down = tool_button("go-down-symbolic", "Move down in the queue", || live::send(Command::Queue(selection(), Queue::Down)));
    for b in [&resume, &pause, &remove] {
        toolbar.append(b);
    }
    // The queue buttons step aside when the window is tight (the menu has them too).
    let queue_box = widgets::hbox(6);
    queue_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    queue_box.append(&up);
    queue_box.append(&down);
    toolbar.append(&queue_box);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Filter by name"));
    search.set_hexpand(true);
    search.set_width_chars(4);
    search.add_css_class("table-search");
    toolbar.append(&search);

    let columns_menu = gtk::MenuButton::new();
    columns_menu.set_icon_name("view-more-horizontal-symbolic");
    columns_menu.set_tooltip_text(Some("Columns"));
    toolbar.append(&columns_menu);
    page.body.append(&toolbar);

    // ----- Model: store -> filter -> sort -> selection -----
    let store = gio::ListStore::new::<TorrentObject>();
    let filter = gtk::CustomFilter::new(|o| o.downcast_ref::<TorrentObject>().is_some_and(|o| o.row().visible));
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let sorter = gtk::CustomSorter::new(|a, b| {
        let (Some(a), Some(b)) = (a.downcast_ref::<TorrentObject>(), b.downcast_ref::<TorrentObject>()) else {
            return gtk::Ordering::Equal;
        };
        a.row().order.cmp(&b.row().order).into()
    });
    let sorted = gtk::SortListModel::new(Some(filtered), Some(sorter.clone()));
    let selection_model = gtk::MultiSelection::new(Some(sorted));
    let cv = gtk::ColumnView::new(Some(selection_model.clone()));
    cv.set_show_column_separators(false);
    cv.set_reorderable(true);
    cv.set_hexpand(true);
    cv.set_vexpand(true);
    cv.add_css_class("torrent-table");

    let mut columns = Vec::new();
    let colbox = widgets::vbox(2);
    for (col, id, title, width) in COLUMNS {
        let c = make_column(*col, title, *width);
        c.set_id(Some(id));
        cv.append_column(&c);
        if *col != Col::Name {
            let label = if *col == Col::Queue { "Queue position" } else { title };
            let check = gtk::CheckButton::with_label(label);
            check.set_active(!p.hidden_columns.iter().any(|h| h == id));
            let id = id.to_string();
            check.connect_toggled(move |b| {
                let on = b.is_active();
                let id = id.clone();
                prefs::update(move |p| {
                    p.hidden_columns.retain(|h| *h != id);
                    if !on {
                        p.hidden_columns.push(id);
                    }
                });
                if let Some(st) = state() {
                    st.borrow_mut().fitted = 0;
                    fit_columns();
                }
            });
            colbox.append(&check);
        }
        columns.push((*col, c));
    }
    let pop = gtk::Popover::new();
    pop.set_child(Some(&colbox));
    columns_menu.set_popover(Some(&pop));
    if let Some(q) = columns.iter().find(|(c, _)| *c == Col::Queue).map(|(_, c)| c.clone()) {
        cv.sort_by_column(Some(&q), gtk::SortType::Ascending);
    }
    if let Some(cvs) = cv.sorter() {
        cvs.connect_changed(|_, _| {
            if let Some(st) = state() {
                st.borrow_mut().order.clear();
                relayout(&st);
            }
        });
    }

    // External, not Never: Never would make the window as wide as every column.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::External)
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&cv)
        .vexpand(true)
        .build();

    let empty_label = widgets::label("", "empty-state");
    empty_label.set_wrap(true);
    empty_label.set_justify(gtk::Justification::Center);
    empty_label.set_xalign(0.5);
    let empty_box = widgets::vbox(12);
    empty_box.set_valign(gtk::Align::Center);
    empty_box.set_halign(gtk::Align::Center);
    empty_box.append(&empty_label);
    let empty_actions = widgets::hbox(8);
    empty_actions.set_halign(gtk::Align::Center);
    let b = gtk::Button::with_label("Add a torrent file");
    b.connect_clicked(|_| actions::open_torrent_files());
    empty_actions.append(&b);
    let b = gtk::Button::with_label("Add a magnet link");
    b.connect_clicked(|_| actions::add_magnet_dialog(""));
    empty_actions.append(&b);
    empty_box.append(&empty_actions);

    let empty = gtk::Stack::new();
    empty.add_named(&scroll, Some("list"));
    empty.add_named(&empty_box, Some("empty"));
    empty.set_visible_child_name("empty");
    empty.set_vexpand(true);

    let card = widgets::vbox(0);
    card.add_css_class("table-card");
    card.set_overflow(gtk::Overflow::Hidden);
    card.append(&empty);
    card.set_vexpand(true);

    // ----- Details pane -----
    let details_widget = details::build();
    let paned = gtk::Paned::new(gtk::Orientation::Vertical);
    paned.add_css_class("transfers-paned");
    paned.set_start_child(Some(&card));
    paned.set_end_child(Some(&details_widget));
    paned.set_resize_start_child(true);
    paned.set_resize_end_child(false);
    paned.set_shrink_start_child(false);
    paned.set_shrink_end_child(false);
    paned.set_vexpand(true);
    page.body.append(&paned);
    // Keep the details pane's height (not its top edge) across window sizes, and
    // remember it when the user drags the divider (not when we place it).
    {
        let ours = Rc::new(std::cell::Cell::new(false));
        let last_h = Rc::new(std::cell::Cell::new(0));
        let (o, lh) = (ours.clone(), last_h.clone());
        paned.add_tick_callback(move |paned, _| {
            let h = paned.height();
            if h > 300 && h != lh.get() {
                let want = prefs::get().details_height.clamp(120, h - 160);
                o.set(true);
                paned.set_position(h - want);
                o.set(false);
                lh.set(h);
            }
            glib::ControlFlow::Continue
        });
        paned.connect_position_notify(move |paned| {
            let h = paned.height();
            // Only a drag moves the divider while the height stays put; GTK's own
            // adjustments come with a resize.
            if ours.get() || h <= 300 || h != last_h.get() || !paned.is_mapped() {
                return;
            }
            let details = (h - paned.position()).max(120);
            thread_local! { static SAVE: std::cell::Cell<Option<glib::SourceId>> = const { std::cell::Cell::new(None) }; }
            if let Some(id) = SAVE.with(|s| s.take()) {
                id.remove();
            }
            let id = glib::timeout_add_local_once(std::time::Duration::from_millis(450), move || {
                SAVE.with(|s| s.set(None));
                prefs::update(|p| p.details_height = details);
            });
            SAVE.with(|s| s.set(Some(id)));
        });
    }

    // ----- Interaction -----
    search.connect_search_changed(|e| {
        if let Some(st) = state() {
            st.borrow_mut().query = e.text().to_string();
            st.borrow_mut().order.clear();
            relayout(&st);
        }
    });
    search.connect_stop_search(|e| e.set_text(""));

    selection_model.connect_selection_changed(|_, _, _| {
        let Some(st_rc) = state() else { return };
        update_buttons(&st_rc);
        sync_details(&st_rc);
    });

    cv.connect_activate(|_, pos| {
        let Some(st) = state() else { return };
        let id = st.borrow().selection.item(pos).and_downcast::<TorrentObject>().map(|o| o.row().s.id.clone());
        if let Some(id) = id {
            actions::open_folder(&id);
        }
    });

    // Right-click: select the row under the pointer (if it isn't already) and show the menu.
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    click.connect_pressed(|g, _, x, y| {
        let Some(cv) = g.widget().and_downcast::<gtk::ColumnView>() else { return };
        let Some(st) = state() else { return };
        let mut w = cv.pick(x, y, gtk::PickFlags::DEFAULT);
        let mut found = None;
        while let Some(widget) = w {
            if let Some((obj, _, _)) = st.borrow().bound.get(&key(&widget)) {
                found = Some(obj.clone());
                break;
            }
            if let Some(child) = widget.first_child()
                && let Some((obj, _, _)) = st.borrow().bound.get(&key(&child))
            {
                found = Some(obj.clone());
                break;
            }
            w = widget.parent();
        }
        let Some(obj) = found else { return };
        let (sel, pos) = {
            let s = st.borrow();
            let mut pos = None;
            for i in 0..s.selection.n_items() {
                if s.selection.item(i).and_downcast::<TorrentObject>().as_ref() == Some(&obj) {
                    pos = Some(i);
                    break;
                }
            }
            (s.selection.clone(), pos)
        };
        if let Some(pos) = pos
            && !sel.is_selected(pos)
        {
            sel.select_item(pos, true);
        }
        actions::context_menu(&cv, x, y, selection());
    });
    cv.add_controller(click);

    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(|_, key, _, mods| {
        let ids = selection();
        if ids.is_empty() {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::Delete | gdk::Key::KP_Delete => {
                actions::remove_dialog(ids);
                glib::Propagation::Stop
            }
            gdk::Key::space => {
                actions::toggle_pause(&ids);
                glib::Propagation::Stop
            }
            gdk::Key::c if mods.contains(gdk::ModifierType::CONTROL_MASK) => {
                actions::copy_magnets(&ids);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    cv.add_controller(keys);

    let st = State {
        store,
        filter,
        sorter,
        selection: selection_model,
        view: cv,
        scroll,
        columns,
        by_id: HashMap::new(),
        bound: HashMap::new(),
        current: Filter::All,
        query: String::new(),
        order: vec!["-".into()],
        search,
        empty,
        empty_label,
        buttons: vec![resume.upcast(), pause.upcast(), remove.upcast(), up.upcast(), down.upcast()],
        fitted: 0,
        queue_box,
        selected_id: None,
    };
    let st = Rc::new(RefCell::new(st));
    // Refit the columns whenever the table's width changes (tiling, resizing).
    st.borrow().view.add_tick_callback(|_, _| {
        fit_columns();
        glib::ControlFlow::Continue
    });
    STATE.with(|s| *s.borrow_mut() = Some(st.clone()));
    update_buttons(&st);
    relayout(&st);
    live::on_update(update);
    if let Some(u) = live::latest() {
        update(&u);
    }
}
