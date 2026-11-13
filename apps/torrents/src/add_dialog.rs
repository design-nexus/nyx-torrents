//! qBittorrent's "Add torrent" dialog: where to save, category, tags, how to
//! start, content layout and which files to download. Magnet links fetch their
//! metadata first (the file list fills in when it arrives).

use crate::engine::{Command, FileEntry, TorrentInfo};
use crate::table::{Choice, Column, OnChoice, Table};
use crate::{actions, fmt, live, paths, prefs, store, widgets};
use gtk::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub enum Source {
    File { data: Vec<u8>, info: TorrentInfo },
    Magnet { uri: String, info: TorrentInfo },
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub info: TorrentInfo,
    pub magnet: String,
    pub torrent: Vec<u8>,
    pub save_path: PathBuf,
    pub category: String,
    pub tags: Vec<String>,
    pub paused: bool,
    pub sequential: bool,
    pub first_last: bool,
    pub skip_check: bool,
    /// original, subfolder or none.
    pub layout: String,
    pub priorities: Vec<u8>,
    /// A new name for the top folder; empty keeps the torrent's.
    pub name: String,
}

pub fn default_request(source: Source) -> Request {
    let p = prefs::get();
    let (info, magnet, torrent) = match source {
        Source::File { data, info } => (info, String::new(), data),
        Source::Magnet { uri, info } => (info, uri, Vec::new()),
    };
    Request {
        info,
        magnet,
        torrent,
        save_path: store::save_path(""),
        paused: p.start_paused,
        layout: p.content_layout.clone(),
        ..Default::default()
    }
}

const PRIORITIES: &[u8] = &[0, 4, 6, 7];

struct Open {
    id: String,
    req: Rc<RefCell<Request>>,
    files: Rc<Table<(usize, FileEntry)>>,
    files_stack: gtk::Stack,
    summary: gtk::Label,
    name_label: gtk::Label,
    rename: gtk::Entry,
    add_button: gtk::Button,
    fetch_label: gtk::Label,
    added: Rc<std::cell::Cell<bool>>,
}

thread_local! {
    static OPEN: RefCell<Vec<Rc<Open>>> = const { RefCell::new(Vec::new()) };
}

fn prio_index(p: u8) -> u32 {
    match p {
        0 => 0,
        1..=4 => 1,
        5 | 6 => 2,
        _ => 3,
    }
}

fn update_summary(o: &Open) {
    let r = o.req.borrow();
    let files: Vec<&FileEntry> = r.info.files.iter().filter(|f| f.priority != 255).collect();
    if files.is_empty() {
        o.summary.set_text("Size unknown until the metadata arrives");
        return;
    }
    let mut n = 0;
    let mut size = 0;
    for (i, f) in r.info.files.iter().enumerate() {
        if f.priority == 255 {
            continue;
        }
        if r.priorities.get(i).copied().unwrap_or(4) > 0 {
            n += 1;
            size += f.size;
        }
    }
    let free = free_space(&r.save_path).map(|b| format!(" · {} free there", fmt::bytes(b as f64))).unwrap_or_default();
    o.summary.set_text(&format!(
        "{n} of {} files · {} of {}{free}",
        files.len(),
        fmt::bytes(size as f64),
        fmt::bytes(r.info.size as f64)
    ));
}

fn free_space(path: &std::path::Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    let c = std::ffi::CString::new(p.to_string_lossy().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a properly sized out-parameter.
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0;
    ok.then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

fn fill_files(o: &Open) {
    let r = o.req.borrow();
    let rows: Vec<(String, (usize, FileEntry))> = r
        .info
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.priority != 255)
        .map(|(i, f)| {
            let mut f = f.clone();
            f.priority = r.priorities.get(i).copied().unwrap_or(4);
            (i.to_string(), (i, f))
        })
        .collect();
    let has = !rows.is_empty();
    drop(r);
    o.files.set(rows);
    o.files_stack.set_visible_child_name(if has { "files" } else { "fetching" });
    update_summary(o);
}

fn toggle(label: &str, active: bool) -> gtk::CheckButton {
    let c = gtk::CheckButton::with_label(label);
    c.set_active(active);
    c
}

pub fn show(source: Source) {
    let is_magnet = matches!(source, Source::Magnet { .. });
    let mut req = default_request(source);
    let id = req.info.id.clone();
    if OPEN.with(|o| o.borrow().iter().any(|x| x.id == id)) {
        return;
    }
    req.priorities = req.info.files.iter().map(|f| if f.priority == 255 { 0 } else { 4 }).collect();
    let req = Rc::new(RefCell::new(req));

    let (dialog, card) = widgets::dialog("Add torrent", 760);
    dialog.set_default_height(680);
    let name_label = widgets::label(&display_name(&req.borrow().info), "add-name");
    name_label.set_wrap(true);
    name_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    card.append(&name_label);
    let summary = widgets::label("", "dim");
    summary.add_css_class("tnum");
    card.append(&summary);

    // ----- Where -----
    let form = gtk::Grid::new();
    form.set_row_spacing(8);
    form.set_column_spacing(12);
    form.add_css_class("add-form");
    let key = |t: &str| {
        let l = widgets::label(t, "kv-key");
        l.set_valign(gtk::Align::Center);
        l
    };
    let (path_field, path_entry) = actions::folder_field(&paths::pretty(&req.borrow().save_path));
    path_field.set_hexpand(true);
    form.attach(&key("Save to"), 0, 0, 1, 1);
    form.attach(&path_field, 1, 0, 1, 1);

    let cats: Vec<String> = store::categories().keys().cloned().collect();
    let mut labels = vec!["None".to_string()];
    labels.extend(cats.iter().cloned());
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let category = gtk::DropDown::from_strings(&label_refs);
    category.set_hexpand(true);
    form.attach(&key("Category"), 0, 1, 1, 1);
    let cat_row = widgets::hbox(6);
    cat_row.append(&category);
    let new_cat = gtk::Button::from_icon_name("list-add-symbolic");
    new_cat.set_tooltip_text(Some("New category"));
    new_cat.connect_clicked(|_| actions::edit_category_dialog(None));
    cat_row.append(&new_cat);
    form.attach(&cat_row, 1, 1, 1, 1);

    let tags = gtk::Entry::new();
    tags.set_placeholder_text(Some("Comma-separated, e.g. linux, iso"));
    form.attach(&key("Tags"), 0, 2, 1, 1);
    form.attach(&tags, 1, 2, 1, 1);

    let rename = gtk::Entry::new();
    rename.set_placeholder_text(Some("Keep the torrent's name"));
    form.attach(&key("Name"), 0, 3, 1, 1);
    form.attach(&rename, 1, 3, 1, 1);

    // Content layout as a segmented control.
    let seg = widgets::hbox(0);
    seg.add_css_class("segmented");
    seg.set_halign(gtk::Align::Start);
    let mut seg_buttons = Vec::new();
    for (value, label) in [("original", "Original"), ("subfolder", "Create subfolder"), ("none", "No subfolder")] {
        let b = gtk::Button::with_label(label);
        if req.borrow().layout == value {
            b.add_css_class("selected");
        }
        seg.append(&b);
        seg_buttons.push((value, b));
    }
    let seg_buttons = Rc::new(seg_buttons);
    for (value, b) in seg_buttons.iter() {
        let (all, req, value) = (seg_buttons.clone(), req.clone(), value.to_string());
        b.connect_clicked(move |_| {
            for (v, other) in all.iter() {
                if *v == value {
                    other.add_css_class("selected");
                } else {
                    other.remove_css_class("selected");
                }
            }
            req.borrow_mut().layout = value.clone();
        });
    }
    form.attach(&key("Layout"), 0, 4, 1, 1);
    form.attach(&seg, 1, 4, 1, 1);
    card.append(&form);

    let opts = gtk::FlowBox::new();
    opts.set_selection_mode(gtk::SelectionMode::None);
    opts.set_max_children_per_line(4);
    opts.set_column_spacing(12);
    let paused = toggle("Start paused", req.borrow().paused);
    let sequential = toggle("Download in order", false);
    let first_last = toggle("First and last pieces first", false);
    let skip = toggle("Skip hash check", false);
    skip.set_tooltip_text(Some("Only when the files are already complete in the save folder."));
    for c in [&paused, &sequential, &first_last, &skip] {
        opts.append(c);
    }
    card.append(&opts);

    // ----- Files -----
    let files_head = widgets::hbox(8);
    let fl = widgets::label("FILES", "group-title");
    fl.set_hexpand(true);
    files_head.append(&fl);
    let all_btn = gtk::Button::with_label("All");
    all_btn.add_css_class("chip");
    all_btn.set_valign(gtk::Align::End);
    let none_btn = gtk::Button::with_label("None");
    none_btn.add_css_class("chip");
    none_btn.set_valign(gtk::Align::End);
    files_head.append(&all_btn);
    files_head.append(&none_btn);
    card.append(&files_head);

    let open_cell: Rc<RefCell<Option<Rc<Open>>>> = Rc::new(RefCell::new(None));
    let on_change: OnChoice = {
        let (req, open_cell) = (req.clone(), open_cell.clone());
        Rc::new(move |key, choice| {
            let (Ok(i), Some(p)) = (key.parse::<usize>(), PRIORITIES.get(choice as usize).copied()) else { return };
            if let Some(slot) = req.borrow_mut().priorities.get_mut(i) {
                *slot = p;
            }
            if let Some(o) = open_cell.borrow().as_ref() {
                update_summary(o);
            }
        })
    };
    let files = Rc::new(Table::new(
        vec![
            Column::text("Name", 0, |f: &(usize, FileEntry)| f.1.path.clone()).sorted(|a, b| a.1.path.cmp(&b.1.path)),
            Column::num("Size", 90, |f| fmt::bytes(f.1.size as f64), |a, b| a.1.size.cmp(&b.1.size)),
            Column {
                title: "Priority",
                width: 140,
                numeric: false,
                text: |_| String::new(),
                compare: None,
                choice: Some(Choice {
                    labels: &["Skip", "Normal", "High", "Maximum"],
                    index: |f| prio_index(f.1.priority),
                    on_change,
                }),
                lead: None,
                button: None,
            },
        ],
        "",
    ));
    let files_stack = gtk::Stack::new();
    let files_card = widgets::vbox(0);
    files_card.add_css_class("table-card");
    files_card.set_overflow(gtk::Overflow::Hidden);
    files_card.append(&files.root);
    files_stack.add_named(&files_card, Some("files"));
    let fetching = widgets::vbox(10);
    fetching.set_valign(gtk::Align::Center);
    let spinner = gtk::Spinner::new();
    spinner.start();
    fetching.append(&spinner);
    let fetch_label = widgets::label("Fetching metadata from peers…", "dim");
    fetch_label.set_xalign(0.5);
    fetch_label.set_wrap(true);
    fetching.append(&fetch_label);
    files_stack.add_named(&fetching, Some("fetching"));
    files_stack.set_vexpand(true);
    files_stack.set_size_request(-1, 200);
    card.append(&files_stack);

    // ----- Buttons -----
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let add =
        gtk::Button::with_label(if is_magnet && req.borrow().info.files.is_empty() { "Add without waiting" } else { "Add" });
    add.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&add);
    card.append(&buttons);

    let open = Rc::new(Open {
        id: id.clone(),
        req: req.clone(),
        files: files.clone(),
        files_stack: files_stack.clone(),
        summary,
        name_label,
        rename: rename.clone(),
        add_button: add.clone(),
        fetch_label,
        added: Rc::new(std::cell::Cell::new(false)),
    });
    *open_cell.borrow_mut() = Some(open.clone());
    OPEN.with(|o| o.borrow_mut().push(open.clone()));
    fill_files(&open);

    // Picking a category moves the save path along, unless the user typed one.
    let path_touched = Rc::new(std::cell::Cell::new(false));
    {
        let (pe, touched) = (path_entry.clone(), path_touched.clone());
        let setting = Rc::new(std::cell::Cell::new(false));
        let s2 = setting.clone();
        pe.connect_changed(move |_| {
            if !s2.get() {
                touched.set(true);
            }
        });
        let (pe, cats, touched) = (path_entry.clone(), cats.clone(), path_touched.clone());
        category.connect_selected_notify(move |dd| {
            if touched.get() {
                return;
            }
            let cat = dd.selected().checked_sub(1).and_then(|i| cats.get(i as usize)).cloned().unwrap_or_default();
            setting.set(true);
            pe.set_text(&paths::pretty(&store::save_path(&cat)));
            setting.set(false);
        });
    }
    {
        let (req, o) = (req.clone(), open.clone());
        path_entry.connect_changed(move |e| {
            req.borrow_mut().save_path = paths::expand(&e.text());
            update_summary(&o);
        });
    }
    for (btn, value) in [(&all_btn, 4u8), (&none_btn, 0u8)] {
        let (req, o) = (req.clone(), open.clone());
        btn.connect_clicked(move |_| {
            {
                let mut r = req.borrow_mut();
                let pads: Vec<bool> = r.info.files.iter().map(|f| f.priority == 255).collect();
                for (i, p) in r.priorities.iter_mut().enumerate() {
                    if !pads.get(i).copied().unwrap_or(false) {
                        *p = value;
                    }
                }
            }
            fill_files(&o);
        });
    }

    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    {
        let (d, o) = (dialog.clone(), open.clone());
        add.connect_clicked(move |_| {
            {
                let mut r = o.req.borrow_mut();
                r.category = category.selected().checked_sub(1).and_then(|i| cats.get(i as usize)).cloned().unwrap_or_default();
                r.tags = tags.text().split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                r.paused = paused.is_active();
                r.sequential = sequential.is_active();
                r.first_last = first_last.is_active();
                r.skip_check = skip.is_active();
                r.name = o.rename.text().trim().to_string();
                r.save_path = paths::expand(&path_entry.text());
                // With metadata, a magnet is added as the fetched .torrent (keeps the file choices).
                if !r.torrent.is_empty() {
                    r.magnet.clear();
                }
                if r.priorities.iter().all(|p| *p == 4) {
                    r.priorities.clear();
                }
            }
            let _ = std::fs::create_dir_all(&o.req.borrow().save_path);
            for t in &o.req.borrow().tags {
                store::add_tag(t);
            }
            o.added.set(true);
            actions::add(o.req.borrow().clone());
            d.close();
        });
    }
    {
        let o = open.clone();
        dialog.connect_close_request(move |_| {
            OPEN.with(|list| list.borrow_mut().retain(|x| !Rc::ptr_eq(x, &o)));
            if is_magnet && !o.added.get() {
                live::send(Command::CancelFetch(o.id.clone()));
            }
            gtk::glib::Propagation::Proceed
        });
    }
    if is_magnet && req.borrow().info.files.is_empty() {
        live::send(Command::Fetch { magnet: req.borrow().magnet.clone() });
    }
    dialog.present();
    add.grab_focus();
}

fn display_name(info: &TorrentInfo) -> String {
    if info.name.is_empty() { format!("Magnet {}", &info.id[..info.id.len().min(12)]) } else { info.name.clone() }
}

/// A magnet's metadata arrived: fill in the dialog waiting for it.
pub fn metadata(id: &str, _name: &str, torrent: &[u8]) {
    let Some(o) = OPEN.with(|list| list.borrow().iter().find(|x| x.id == id).cloned()) else { return };
    let Ok(info) = crate::engine::ffi::parse_torrent(torrent) else { return };
    {
        let mut r = o.req.borrow_mut();
        r.priorities = info.files.iter().map(|f| if f.priority == 255 { 0 } else { 4 }).collect();
        r.info = info;
        r.torrent = torrent.to_vec();
    }
    o.name_label.set_text(&display_name(&o.req.borrow().info));
    o.add_button.set_label("Add");
    fill_files(&o);
}

pub fn fetch_failed(id: &str, error: &str) {
    if let Some(o) = OPEN.with(|list| list.borrow().iter().find(|x| x.id == id).cloned()) {
        o.fetch_label.set_text(&format!("Couldn't fetch the metadata: {error}"));
    }
}
