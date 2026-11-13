//! Nyx-style building blocks: pages, groups and option rows.

pub use nyx_ui::widgets::*;

use gtk::pango;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

// ---------- Search registry ----------

pub struct SearchItem {
    pub section: String,
    pub text: String,
    pub row: gtk::Widget,
    pub group: Option<gtk::Widget>,
}

thread_local! {
    static CURRENT_SECTION: RefCell<String> = const { RefCell::new(String::new()) };
    static CURRENT_GROUP: RefCell<Option<gtk::Widget>> = const { RefCell::new(None) };
    pub static SEARCH: RefCell<Vec<SearchItem>> = const { RefCell::new(Vec::new()) };
}

fn register(row: &impl IsA<gtk::Widget>, title: &str, desc: &str, keywords: &str) {
    let section = CURRENT_SECTION.with(|s| s.borrow().clone());
    let group = CURRENT_GROUP.with(|g| g.borrow().clone());
    SEARCH.with(|s| {
        s.borrow_mut().push(SearchItem {
            section,
            text: format!("{title} {desc} {keywords}").to_lowercase(),
            row: row.clone().upcast(),
            group,
        })
    });
}

// ---------- Page / group ----------

pub struct Page {
    pub root: gtk::ScrolledWindow,
    pub body: gtk::Box,
}

/// A page. Its name shows in the top bar (or the settings dialog's header), so
/// the page itself starts straight with its content.
pub fn page(section: &str) -> Page {
    CURRENT_SECTION.with(|s| *s.borrow_mut() = section.to_string());
    CURRENT_GROUP.with(|g| *g.borrow_mut() = None);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.add_css_class("settings-page");
    body.add_css_class(&format!("page-{section}"));
    body.set_hexpand(true);

    let root = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .child(&body)
        .vexpand(true)
        .build();
    Page { root, body }
}

impl Page {
    /// For pages whose content scrolls itself (tables): the page stops scrolling
    /// and hands its height to the last child.
    pub fn fill(&self) {
        self.root.set_vscrollbar_policy(gtk::PolicyType::Never);
        self.body.set_vexpand(true);
        self.body.add_css_class("fill");
    }

    pub fn group(&self, title: &str) -> Group {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 0);
        wrapper.add_css_class("settings-group");
        wrapper.set_widget_name(title);
        if !title.is_empty() {
            let l = gtk::Label::new(Some(&title.to_uppercase()));
            l.add_css_class("group-title");
            l.set_xalign(0.0);
            wrapper.append(&l);
        }
        // Rows join into one card, split by hairlines.
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("group-list");
        wrapper.append(&list);
        self.body.append(&wrapper);
        CURRENT_GROUP.with(|g| *g.borrow_mut() = Some(wrapper.clone().upcast()));
        Group { wrapper, list }
    }
}

#[derive(Clone)]
pub struct Group {
    pub wrapper: gtk::Box,
    pub list: gtk::Box,
}

impl Group {
    pub fn add(&self, w: &impl IsA<gtk::Widget>) {
        self.list.append(w);
    }

    pub fn note(&self, text: &str) {
        let l = gtk::Label::new(None);
        l.set_markup(text);
        l.add_css_class("group-note");
        l.set_xalign(0.0);
        l.set_wrap(true);
        // Notes sit just under the group title.
        self.wrapper.insert_child_after(&l, self.wrapper.first_child().as_ref());
    }
}

// ---------- Rows ----------

/// An option card: title and description on the left, control on the right.
pub fn row(title: &str, desc: &str, control: Option<&gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.add_css_class("settings-option");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    let t = gtk::Label::new(Some(title));
    t.add_css_class("settings-option-title");
    t.set_xalign(0.0);
    t.set_wrap(true);
    text.append(&t);
    if !desc.is_empty() {
        let d = gtk::Label::new(None);
        d.set_markup(desc);
        d.add_css_class("settings-option-description");
        d.set_xalign(0.0);
        d.set_wrap(true);
        d.set_wrap_mode(pango::WrapMode::WordChar);
        text.append(&d);
    }
    row.append(&text);
    if let Some(c) = control {
        c.set_valign(gtk::Align::Center);
        row.append(c);
    }
    register(&row, title, desc, "");
    row
}

pub fn switch_row(title: &str, desc: &str, active: bool, on_change: impl Fn(bool) + 'static) -> (gtk::Box, gtk::Switch) {
    let sw = gtk::Switch::new();
    sw.set_active(active);
    sw.connect_active_notify(move |s| on_change(s.is_active()));
    let r = row(title, desc, Some(sw.upcast_ref()));
    (r, sw)
}

pub fn choice_row(
    title: &str,
    desc: &str,
    options: Vec<(String, String)>,
    current: &str,
    on_change: impl Fn(String) + 'static,
) -> (gtk::Box, gtk::DropDown) {
    let dd = dropdown(&options, current);
    dd.connect_selected_notify(move |d| {
        if let Some((id, _)) = options.get(d.selected() as usize) {
            on_change(id.clone());
        }
    });
    let r = row(title, desc, Some(dd.upcast_ref()));
    (r, dd)
}

pub fn button_row(title: &str, desc: &str, label: &str, on_click: impl Fn(&gtk::Button) + 'static) -> (gtk::Box, gtk::Button) {
    let b = gtk::Button::with_label(label);
    b.connect_clicked(on_click);
    let r = row(title, desc, Some(b.upcast_ref()));
    (r, b)
}

// ---------- Monitoring pieces ----------

/// A card holding a graph: title on the left, a live summary on the right.
pub fn graph_card(title: &str, graph: &impl IsA<gtk::Widget>) -> (gtk::Box, gtk::Label) {
    let card = vbox(10);
    card.add_css_class("graph-card");
    let head = hbox(10);
    let t = label(title, "graph-card-title");
    t.set_hexpand(true);
    t.set_ellipsize(pango::EllipsizeMode::End);
    let summary = label("", "dim");
    summary.add_css_class("tnum");
    summary.set_xalign(1.0);
    head.append(&t);
    head.append(&summary);
    card.append(&head);
    card.append(graph);
    register(&card, title, "", "");
    (card, summary)
}

// ---------- More controls ----------

/// A number with a unit readout: KiB/s, minutes, connections.
#[allow(clippy::too_many_arguments)]
pub fn spin_row(
    title: &str,
    desc: &str,
    range: (f64, f64),
    step: f64,
    digits: u32,
    value: f64,
    unit: &str,
    on_change: impl Fn(f64) + 'static,
) -> (gtk::Box, gtk::SpinButton) {
    let spin = gtk::SpinButton::with_range(range.0, range.1, step);
    spin.set_digits(digits);
    spin.set_value(value);
    spin.add_css_class("tnum");
    spin.set_valign(gtk::Align::Center);
    spin.connect_value_changed(move |s| on_change(s.value()));
    let bx = hbox(8);
    bx.append(&spin);
    if !unit.is_empty() {
        let u = label(unit, "dim");
        u.add_css_class("tnum");
        u.set_width_chars(6);
        bx.append(&u);
    }
    let r = row(title, desc, Some(bx.upcast_ref()));
    (r, spin)
}

pub fn entry_row(
    title: &str,
    desc: &str,
    value: &str,
    placeholder: &str,
    mono: bool,
    on_change: impl Fn(String) + 'static,
) -> (gtk::Box, gtk::Entry) {
    let e = gtk::Entry::new();
    e.set_text(value);
    e.set_placeholder_text(Some(placeholder));
    e.set_width_chars(18);
    if mono {
        e.add_css_class("mono");
    }
    e.connect_changed(move |e| on_change(e.text().to_string()));
    let r = row(title, desc, Some(e.upcast_ref()));
    (r, e)
}

/// A card with a wide control under the text (paths, lists).
pub fn stacked_row(title: &str, desc: &str, control: &impl IsA<gtk::Widget>) -> gtk::Box {
    let r = row(title, desc, None);
    r.add_css_class("tall");
    r.set_orientation(gtk::Orientation::Vertical);
    r.set_spacing(10);
    control.set_hexpand(true);
    r.append(control);
    r
}

/// A pill track of a few mutually exclusive choices.
pub fn segmented(options: &[(&str, &str)], current: &str, on_change: impl Fn(String) + 'static) -> gtk::Box {
    let seg = hbox(0);
    seg.add_css_class("segmented");
    seg.set_valign(gtk::Align::Center);
    let on_change = Rc::new(on_change);
    let buttons: Rc<RefCell<Vec<(String, gtk::Button)>>> = Rc::new(RefCell::new(Vec::new()));
    for (value, text) in options {
        let b = gtk::Button::with_label(text);
        if *value == current {
            b.add_css_class("selected");
        }
        let (all, f, v) = (buttons.clone(), on_change.clone(), value.to_string());
        b.connect_clicked(move |_| {
            for (id, other) in all.borrow().iter() {
                if *id == v {
                    other.add_css_class("selected");
                } else {
                    other.remove_css_class("selected");
                }
            }
            f(v.clone());
        });
        seg.append(&b);
        buttons.borrow_mut().push((value.to_string(), b));
    }
    seg
}

pub fn segmented_row(
    title: &str,
    desc: &str,
    options: &[(&str, &str)],
    current: &str,
    on_change: impl Fn(String) + 'static,
) -> gtk::Box {
    let seg = segmented(options, current, on_change);
    row(title, desc, Some(seg.upcast_ref()))
}

// ---------- File tiles ----------

/// What a torrent most likely holds, going by its name: an icon and the swatch its
/// tile is painted in.
pub fn file_kind(name: &str) -> (&'static str, &'static str) {
    const DISC: (&str, &str) = ("media-optical-symbolic", "blue");
    const FILM: (&str, &str) = ("video-x-generic-symbolic", "coral");
    const AUDIO: (&str, &str) = ("audio-x-generic-symbolic", "teal");
    const BOOK: (&str, &str) = ("text-x-generic-symbolic", "amber");
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "iso" | "img" | "dmg" | "qcow2" => return DISC,
        "mkv" | "mp4" | "avi" | "mov" | "webm" | "m4v" => return FILM,
        "mp3" | "flac" | "ogg" | "opus" | "m4a" | "m4b" | "wav" => return AUDIO,
        "epub" | "pdf" | "mobi" | "azw3" | "cbz" | "cbr" => return BOOK,
        _ => {}
    }
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&["1080p", "720p", "2160p", "bluray", "blu-ray", "webrip", "web-dl", "hdtv", "x264", "x265", "hevc", "dvdrip"]) {
        FILM
    } else if has(&["audiobook", "librivox", "flac", "mp3", "discography"]) {
        AUDIO
    } else if has(&["epub", "ebook"]) {
        BOOK
    } else if has(&["amd64", "x86_64", "arm64", "aarch64", "i386", "linux", "-dvd-", "netinst"]) {
        DISC
    } else {
        ("package-x-generic-symbolic", "violet")
    }
}

/// A rounded tile in a swatch with the kind's icon in white.
pub fn file_tile(name: &str) -> gtk::Box {
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("file-tile");
    tile.set_valign(gtk::Align::Center);
    tile.set_hexpand(false);
    let image = gtk::Image::new();
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    tile.append(&image);
    tile.set_halign(gtk::Align::Start);
    set_file_tile(tile.upcast_ref(), name);
    tile
}

pub fn set_file_tile(tile: &gtk::Widget, name: &str) {
    let (icon, swatch) = file_kind(name);
    let class = format!("section-swatch-{swatch}");
    if tile.has_css_class(&class) {
        return;
    }
    for c in tile.css_classes() {
        if c.starts_with("section-swatch-") {
            tile.remove_css_class(&c);
        }
    }
    tile.add_css_class(&class);
    if let Some(image) = tile.first_child().and_downcast::<gtk::Image>() {
        image.set_icon_name(Some(icon));
    }
}

/// Key caps joined with a dim "+".
pub fn keycaps(keys: &[&str]) -> gtk::Box {
    let caps = hbox(4);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            caps.append(&label("+", "dim"));
        }
        caps.append(&label(k, "key-cap"));
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_what_a_torrent_holds() {
        assert_eq!(file_kind("debian-13.2.0-amd64-DVD-1.iso").1, "blue");
        assert_eq!(file_kind("Nosferatu (1922) restored 1080p").1, "coral");
        assert_eq!(file_kind("Pride and Prejudice, LibriVox audiobook").1, "teal");
        assert_eq!(file_kind("archlinux-2026.11.01-x86_64").1, "blue");
        assert_eq!(file_kind("some photos").1, "violet");
    }
}
