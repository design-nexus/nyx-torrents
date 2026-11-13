//! A small live table for the details pane (peers, trackers, files). Rows are
//! keyed; each update changes their data in place and re-renders the visible
//! cells, so scrolling and selection survive the once-a-second refresh.

use crate::widgets;
use gtk::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

pub type Text<T> = fn(&T) -> String;
pub type Compare<T> = fn(&T, &T) -> Ordering;
/// Called with the row's key and the chosen option's index.
pub type OnChoice = Rc<dyn Fn(&str, u32)>;
/// Called with the row's key.
pub type OnClick = Rc<dyn Fn(&str)>;

pub struct Column<T> {
    pub title: &'static str,
    /// 0 expands.
    pub width: i32,
    pub numeric: bool,
    pub text: Text<T>,
    pub compare: Option<Compare<T>>,
    /// A dropdown instead of text: (labels, value of row, on change(row key, choice index)).
    pub choice: Option<Choice<T>>,
    /// Something before the text.
    pub lead: Option<Lead<T>>,
    /// An icon button instead of text: (icon, tooltip, on click). The column never hides.
    pub button: Option<(&'static str, &'static str, OnClick)>,
}

pub enum Lead<T> {
    /// The file tile for the name this gives (see `widgets::file_tile`).
    Tile(Text<T>),
    /// A small dot with the class this picks.
    Dot(fn(&T) -> &'static str),
}

pub struct Choice<T> {
    pub labels: &'static [&'static str],
    pub index: fn(&T) -> u32,
    pub on_change: OnChoice,
}

impl<T> Column<T> {
    pub fn text(title: &'static str, width: i32, text: Text<T>) -> Self {
        Column { title, width, numeric: false, text, compare: None, choice: None, lead: None, button: None }
    }
    pub fn num(title: &'static str, width: i32, text: Text<T>, compare: Compare<T>) -> Self {
        Column { title, width, numeric: true, text, compare: Some(compare), choice: None, lead: None, button: None }
    }
    pub fn button(width: i32, icon: &'static str, tooltip: &'static str, on_click: OnClick) -> Self {
        Column { button: Some((icon, tooltip, on_click)), ..Column::text("", width, |_| String::new()) }
    }
    pub fn lead(mut self, lead: Lead<T>) -> Self {
        self.lead = Some(lead);
        self
    }
    pub fn sorted(mut self, compare: Compare<T>) -> Self {
        self.compare = Some(compare);
        self
    }
}

struct Item<T> {
    key: String,
    value: T,
}

type Obj = glib::BoxedAnyObject;
type Bound = HashMap<usize, (Obj, usize, gtk::Widget)>;

pub struct Table<T: 'static> {
    pub root: gtk::Box,
    pub view: gtk::ColumnView,
    store: gio::ListStore,
    sorter: Option<gtk::Sorter>,
    by_key: RefCell<HashMap<String, Obj>>,
    bound: Rc<RefCell<Bound>>,
    columns: Rc<Vec<Column<T>>>,
    empty: gtk::Stack,
    empty_label: gtk::Label,
    /// Room the expanding column keeps before columns at the right start to hide.
    room: Rc<std::cell::Cell<i32>>,
}

fn key(w: &gtk::Widget) -> usize {
    w.as_ptr() as usize
}

fn render<T: 'static>(columns: &[Column<T>], obj: &Obj, col: usize, w: &gtk::Widget) {
    let item = obj.borrow::<Item<T>>();
    let c = &columns[col];
    if let Some(choice) = &c.choice {
        if let Some(dd) = w.downcast_ref::<gtk::DropDown>() {
            let i = (choice.index)(&item.value);
            if dd.selected() != i {
                // Mark the change as ours so the handler doesn't echo it back.
                unsafe { dd.set_data("rendering", true) };
                dd.set_selected(i);
                unsafe { dd.set_data("rendering", false) };
            }
            unsafe { dd.set_data("row-key", item.key.clone()) };
        }
        return;
    }
    if c.button.is_some() {
        unsafe { w.set_data("row-key", item.key.clone()) };
        return;
    }
    let label = match &c.lead {
        Some(lead) => {
            let Some(first) = w.first_child() else { return };
            match lead {
                Lead::Tile(name) => widgets::set_file_tile(&first, &name(&item.value)),
                Lead::Dot(class) => {
                    for old in first.css_classes() {
                        if old != "cell-dot" {
                            first.remove_css_class(&old);
                        }
                    }
                    first.add_css_class(class(&item.value));
                }
            }
            w.last_child()
        }
        None => Some(w.clone()),
    };
    if let Some(l) = label.and_downcast_ref::<gtk::Label>() {
        let text = (c.text)(&item.value);
        if l.text() != text {
            l.set_text(&text);
            if !c.numeric {
                l.set_tooltip_text(Some(&text));
            }
        }
    }
}

impl<T: 'static> Table<T> {
    pub fn new(columns: Vec<Column<T>>, empty_text: &str) -> Table<T> {
        let columns = Rc::new(columns);
        let bound: Rc<RefCell<Bound>> = Rc::new(RefCell::new(HashMap::new()));
        let store = gio::ListStore::new::<Obj>();
        let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        let selection = gtk::NoSelection::new(Some(sorted.clone()));
        let view = gtk::ColumnView::new(Some(selection));
        view.set_show_column_separators(false);
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.add_css_class("details-table");
        // (column, width, never hides)
        let mut fit: Vec<(gtk::ColumnViewColumn, i32, bool)> = Vec::new();
        for (i, c) in columns.iter().enumerate() {
            let factory = gtk::SignalListItemFactory::new();
            let numeric = c.numeric;
            let choice_labels = c.choice.as_ref().map(|ch| (ch.labels, ch.on_change.clone()));
            let button = c.button.as_ref().map(|(icon, tip, f)| (*icon, *tip, f.clone()));
            let lead = c.lead.as_ref().map(|l| matches!(l, Lead::Tile(_)));
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                let w: gtk::Widget = if let Some((labels, on_change)) = &choice_labels {
                    let dd = gtk::DropDown::from_strings(labels);
                    dd.add_css_class("cell-choice");
                    let on_change = on_change.clone();
                    dd.connect_selected_notify(move |dd| {
                        let rendering = unsafe { dd.data::<bool>("rendering").map(|p| *p.as_ref()) }.unwrap_or(false);
                        if rendering {
                            return;
                        }
                        if let Some(k) = unsafe { dd.data::<String>("row-key").map(|p| p.as_ref().clone()) } {
                            on_change(&k, dd.selected());
                        }
                    });
                    dd.upcast()
                } else if let Some((icon, tip, on_click)) = &button {
                    let b = widgets::icon_button(icon, tip);
                    b.add_css_class("cell-button");
                    b.set_halign(gtk::Align::End);
                    let on_click = on_click.clone();
                    b.connect_clicked(move |b| {
                        if let Some(k) = unsafe { b.data::<String>("row-key").map(|p| p.as_ref().clone()) } {
                            on_click(&k);
                        }
                    });
                    b.upcast()
                } else {
                    let l = gtk::Label::new(None);
                    l.set_ellipsize(pango::EllipsizeMode::End);
                    if numeric {
                        l.add_css_class("cell-num");
                        l.set_xalign(1.0);
                    } else {
                        l.set_xalign(0.0);
                    }
                    match lead {
                        Some(tile) => {
                            let b = widgets::hbox(if tile { 10 } else { 8 });
                            if tile {
                                b.append(&widgets::file_tile(""));
                            } else {
                                let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                                dot.add_css_class("cell-dot");
                                dot.set_valign(gtk::Align::Center);
                                b.append(&dot);
                            }
                            // A dot sits right beside its number.
                            l.set_hexpand(tile);
                            if !tile {
                                b.set_halign(gtk::Align::End);
                            }
                            b.append(&l);
                            b.upcast()
                        }
                        None => l.upcast(),
                    }
                };
                item.set_child(Some(&w));
            });
            let (b, cols) = (bound.clone(), columns.clone());
            factory.connect_bind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                let (Some(obj), Some(w)) = (item.item().and_downcast::<Obj>(), item.child()) else { return };
                render(&cols, &obj, i, &w);
                b.borrow_mut().insert(key(&w), (obj, i, w));
            });
            let b = bound.clone();
            factory.connect_unbind(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                if let Some(w) = item.child() {
                    b.borrow_mut().remove(&key(&w));
                }
            });
            let col = gtk::ColumnViewColumn::new(Some(c.title), Some(factory));
            col.set_resizable(true);
            if c.width > 0 {
                col.set_fixed_width(c.width);
            } else {
                col.set_expand(true);
            }
            if let Some(cmp) = c.compare {
                col.set_sorter(Some(&gtk::CustomSorter::new(move |a, b| {
                    let (Some(a), Some(b)) = (a.downcast_ref::<Obj>(), b.downcast_ref::<Obj>()) else {
                        return gtk::Ordering::Equal;
                    };
                    cmp(&a.borrow::<Item<T>>().value, &b.borrow::<Item<T>>().value).into()
                })));
            }
            view.append_column(&col);
            fit.push((col, c.width, c.width == 0 || c.button.is_some()));
        }
        // When the table narrows, hide columns from the right (the expanding one stays).
        // Measure the scrolled window around it: the view itself never gets narrower
        // than its columns.
        let fitted = std::cell::Cell::new(0);
        let room = Rc::new(std::cell::Cell::new(200));
        let r = room.clone();
        view.add_tick_callback(move |view, _| {
            let width = view.parent().map(|p| p.width()).unwrap_or(0);
            if width > 0 && width != fitted.get() {
                fitted.set(width);
                let mut used: i32 = r.get() + fit.iter().map(|(_, w, _)| *w).sum::<i32>();
                for (col, w, keep) in fit.iter().rev() {
                    let show = *keep || used <= width;
                    if !show {
                        used -= w;
                    }
                    if col.is_visible() != show {
                        col.set_visible(show);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
        let sorter = view.sorter();
        sorted.set_sorter(sorter.as_ref());

        // External, not Never: Never would make the window as wide as every column.
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&view)
            .vexpand(true)
            .build();
        let empty_label = widgets::label(empty_text, "empty-state");
        empty_label.set_xalign(0.5);
        empty_label.set_wrap(true);
        let empty = gtk::Stack::new();
        empty.add_named(&scroll, Some("list"));
        empty.add_named(&empty_label, Some("empty"));
        empty.set_visible_child_name("empty");
        let root = widgets::vbox(0);
        root.append(&empty);
        root.set_vexpand(true);
        Table { root, view, store, sorter, by_key: RefCell::new(HashMap::new()), bound, columns, empty, empty_label, room }
    }

    pub fn set_room(&self, px: i32) {
        self.room.set(px);
    }

    pub fn set_empty_text(&self, text: &str) {
        self.empty_label.set_text(text);
    }

    /// Replace the rows: same keys keep their row objects.
    pub fn set(&self, items: Vec<(String, T)>) {
        let mut by_key = self.by_key.borrow_mut();
        let mut seen = std::collections::HashSet::new();
        let mut added = Vec::new();
        let mut changed = false;
        for (k, value) in items {
            seen.insert(k.clone());
            if let Some(obj) = by_key.get(&k) {
                obj.borrow_mut::<Item<T>>().value = value;
            } else {
                let obj = Obj::new(Item { key: k.clone(), value });
                by_key.insert(k, obj.clone());
                added.push(obj);
            }
        }
        let gone: Vec<String> = by_key.keys().filter(|k| !seen.contains(*k)).cloned().collect();
        for k in gone {
            if let Some(obj) = by_key.remove(&k)
                && let Some(pos) = self.store.find(&obj)
            {
                self.store.remove(pos);
                changed = true;
            }
        }
        if !added.is_empty() {
            self.store.extend_from_slice(&added);
            changed = true;
        }
        let empty = by_key.is_empty();
        drop(by_key);
        self.empty.set_visible_child_name(if empty { "empty" } else { "list" });
        let bound: Vec<(Obj, usize, gtk::Widget)> = self.bound.borrow().values().cloned().collect();
        for (obj, col, w) in bound {
            render(&self.columns, &obj, col, &w);
        }
        // Keep a sorted column sorted as values change.
        if let Some(s) = &self.sorter
            && (changed || s.order() != gtk::SorterOrder::None)
        {
            s.changed(gtk::SorterChange::Different);
        }
    }

    pub fn clear(&self) {
        self.by_key.borrow_mut().clear();
        self.store.remove_all();
        self.empty.set_visible_child_name("empty");
    }

    /// The row object under a point in the view (for right-click menus).
    fn obj_at(&self, x: f64, y: f64) -> Option<Obj> {
        let mut w = self.view.pick(x, y, gtk::PickFlags::DEFAULT);
        let bound = self.bound.borrow();
        while let Some(widget) = w {
            if let Some((obj, _, _)) = bound.get(&key(&widget)) {
                return Some(obj.clone());
            }
            if let Some(child) = widget.first_child()
                && let Some((obj, _, _)) = bound.get(&key(&child))
            {
                return Some(obj.clone());
            }
            w = widget.parent();
        }
        None
    }
}

impl<T: Clone + 'static> Table<T> {
    /// Double-click or Enter on a row.
    pub fn on_activate(&self, f: impl Fn(T) + 'static) {
        self.view.connect_activate(move |view, pos| {
            if let Some(obj) = view.model().and_then(|m| m.item(pos)).and_downcast::<Obj>() {
                let value = obj.borrow::<Item<T>>().value.clone();
                f(value);
            }
        });
    }

    /// Right-click on a row: `f(view, value, x, y)`.
    pub fn on_secondary(self: &Rc<Self>, f: impl Fn(&gtk::ColumnView, T, f64, f64) + 'static) {
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, x, y| {
            let Some(t) = weak.upgrade() else { return };
            if let Some(obj) = t.obj_at(x, y) {
                let value = obj.borrow::<Item<T>>().value.clone();
                f(&t.view, value, x, y);
            }
        });
        self.view.add_controller(click);
    }
}
