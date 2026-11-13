//! What the user can do to torrents, and what happens on its own: adding
//! (files, magnets, links), removing, the context menu, categories and tags,
//! and reacting to engine events (finished downloads, errors, ratio limits).

use crate::add_dialog::{self, Source};
use crate::engine::{self, AddParams, Command, Event, Queue, Update};
use crate::model::{self, Filter};
use crate::sections::transfers;
use crate::store::{self, Category};
use crate::widgets;
use crate::{cmd, live, paths, prefs, window};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

thread_local! {
    /// Torrents we've already applied the ratio/seeding-time action to.
    static LIMITED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// .torrent files to delete once their torrent is added.
    static DELETE_AFTER: RefCell<Vec<(String, PathBuf)>> = const { RefCell::new(Vec::new()) };
}

pub fn install(_app: &gtk::Application) {
    live::on_event(handle_event);
    live::on_update(check_limits);
}

// ---------- Engine events ----------

fn name_of(id: &str) -> String {
    transfers::row(id).map(|r| r.s.name).unwrap_or_else(|| "A torrent".into())
}

fn handle_event(e: &Event) {
    match e {
        Event::AddFailed { id, error } => {
            if !id.is_empty() && transfers::row(id).is_none() {
                store::forget(id);
            }
            add_dialog::fetch_failed(id, error);
            window::toast(&format!("Couldn't add the torrent: {error}"));
        }
        Event::Metadata { id, name, torrent } => add_dialog::metadata(id, name, torrent),
        Event::Finished { id, name } => finished(id, name),
        Event::Error { id, text } => window::toast(&format!("{}: {text}", name_of(id))),
        Event::Moved { id } => {
            let mut m = store::meta(id);
            if m.target.take().is_some() {
                store::set_meta(id, m);
            }
        }
        Event::MoveFailed { id, text } => window::toast(&format!("Couldn't move {}: {text}", name_of(id))),
        Event::DeleteFailed { id, text } => window::toast(&format!("Couldn't delete the files of {}: {text}", name_of(id))),
        Event::Removed { id } => {
            store::forget(id);
            LIMITED.with(|l| l.borrow_mut().remove(id));
        }
        Event::Warning { text } => window::toast(text),
        // Piece priorities need the file list, which a magnet only now has.
        Event::MetadataReceived { id } => {
            if store::meta(id).first_last {
                live::send(Command::FirstLast(vec![id.clone()], true));
            }
        }
        Event::Restored => {}
    }
}

fn finished(id: &str, name: &str) {
    let p = prefs::get();
    let meta = store::meta(id);
    // Downloaded into the incomplete folder: move it where it belongs.
    if let Some(target) = &meta.target {
        live::send(Command::Move(vec![id.to_string()], target.clone()));
    }
    // Resume data restored at startup reports "finished" for complete torrents; only
    // announce ones that finished while we watched.
    let fresh = transfers::row(id).is_some_and(|r| !model::done(&r.s) || r.s.completed > now() - 120);
    if !fresh {
        return;
    }
    if p.notify_finished {
        window::notify("Download finished", name);
    }
    if !p.on_finish.trim().is_empty() {
        let save = meta.target.clone().or_else(|| transfers::row(id).map(|r| r.s.save_path)).unwrap_or_default();
        let content = Path::new(&save).join(name);
        let command = p
            .on_finish
            .replace("%N", &shell_quote(name))
            .replace("%F", &shell_quote(&content.to_string_lossy()))
            .replace("%D", &shell_quote(&save))
            .replace("%I", id)
            .replace("%L", &shell_quote(&meta.category));
        cmd::spawn(&["sh", "-c", &command]);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Stop seeding at the share ratio or seeding time limit, like qBittorrent.
fn check_limits(u: &Update) {
    let p = prefs::get();
    if p.ratio_limit <= 0.0 && p.seed_time_limit <= 0 {
        return;
    }
    for s in &u.torrents {
        if !model::done(s) || model::stopped(s) || s.super_seeding {
            continue;
        }
        let over_ratio = p.ratio_limit > 0.0 && model::ratio(s) >= p.ratio_limit;
        let over_time = p.seed_time_limit > 0 && s.seeding_secs >= p.seed_time_limit * 60;
        if !(over_ratio || over_time) || LIMITED.with(|l| l.borrow().contains(&s.id)) {
            continue;
        }
        LIMITED.with(|l| l.borrow_mut().insert(s.id.clone()));
        let ids = vec![s.id.clone()];
        match p.limit_action.as_str() {
            "remove" => live::send(Command::Remove { ids, with_files: false }),
            "remove-files" => live::send(Command::Remove { ids, with_files: true }),
            "superseed" => live::send(Command::SuperSeed(ids, true)),
            _ => live::send(Command::Pause(ids)),
        }
    }
}

// ---------- Adding ----------

pub fn open_torrent_files() {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Torrent files"));
    filter.add_pattern("*.torrent");
    filter.add_mime_type("application/x-bittorrent");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder().title("Add torrent files").modal(true).filters(&filters).build();
    let parent = window::window();
    dialog.open_multiple(parent.as_ref(), gio::Cancellable::NONE, |res| {
        let Ok(files) = res else { return };
        for i in 0..files.n_items() {
            if let Some(path) = files.item(i).and_downcast::<gio::File>().and_then(|f| f.path()) {
                add_torrent_path(&path);
            }
        }
    });
}

pub fn add_torrent_path(path: &Path) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            window::toast(&format!("Couldn't read {}: {e}", paths::pretty(path)));
            return;
        }
    };
    add_torrent_data(data, Some(path.to_path_buf()));
}

pub fn add_torrent_data(data: Vec<u8>, path: Option<PathBuf>) {
    match engine::ffi::parse_torrent(&data) {
        Ok(info) => {
            if transfers::row(&info.id).is_some() {
                window::toast(&format!("{} is already in the list.", info.name));
                return;
            }
            if let Some(p) = path
                && prefs::get().delete_torrent_file
            {
                DELETE_AFTER.with(|d| d.borrow_mut().push((info.id.clone(), p)));
            }
            let source = Source::File { data, info };
            if prefs::get().show_add_dialog {
                add_dialog::show(source);
            } else {
                add(add_dialog::default_request(source));
            }
        }
        Err(e) => window::toast(&format!("That isn't a torrent file ({}).", e.what())),
    }
}

/// A magnet link, an http(s) link to a .torrent, a file:// URI, or a bare info-hash.
pub fn add_link(text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    let hex = t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit());
    if t.starts_with("magnet:") || hex {
        let uri = if hex { format!("magnet:?xt=urn:btih:{t}") } else { t.to_string() };
        add_magnet(&uri);
    } else if t.starts_with("file://") {
        if let Some(p) = gio::File::for_uri(t).path() {
            add_torrent_path(&p);
        }
    } else if t.starts_with("http://") || t.starts_with("https://") {
        download_torrent(t);
    } else if Path::new(t).is_file() {
        add_torrent_path(Path::new(t));
    } else {
        window::toast("That isn't a magnet link, a torrent file or a link to one.");
    }
}

fn add_magnet(uri: &str) {
    match engine::ffi::parse_magnet(uri) {
        Ok(info) => {
            if transfers::row(&info.id).is_some() {
                window::toast("That torrent is already in the list.");
                return;
            }
            let source = Source::Magnet { uri: uri.to_string(), info };
            if prefs::get().show_add_dialog {
                add_dialog::show(source);
            } else {
                add(add_dialog::default_request(source));
            }
        }
        Err(e) => window::toast(e.what()),
    }
}

/// Fetch a .torrent over http(s) with curl, off the UI thread.
pub fn download_torrent(url: &str) {
    if !cmd::present("curl") {
        window::toast("Downloading .torrent links needs curl.");
        return;
    }
    window::toast("Downloading the torrent file…");
    let url = url.to_string();
    cmd::background(
        move || {
            let out = std::process::Command::new("curl")
                .args(["-fsSL", "--max-time", "60", "--max-filesize", "50000000", "-A", engine::settings::USER_AGENT, &url])
                .output();
            match out {
                Ok(o) if o.status.success() => Ok(o.stdout),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
                Err(e) => Err(e.to_string()),
            }
        },
        |res: Result<Vec<u8>, String>| match res {
            // Some sites redirect .torrent links to magnets.
            Ok(data) if data.starts_with(b"magnet:") => add_link(&String::from_utf8_lossy(&data)),
            Ok(data) => add_torrent_data(data, None),
            Err(e) => window::toast(&format!("Couldn't download the torrent: {e}")),
        },
    );
}

/// Add with the options from the add dialog (or the defaults).
pub fn add(req: add_dialog::Request) {
    let p = prefs::get();
    let id = req.info.id.clone();
    let final_path = req.save_path.clone();
    let mut meta =
        store::Meta { category: req.category.clone(), tags: req.tags.clone(), target: None, first_last: req.first_last };
    let incomplete = paths::expand(&p.incomplete_path);
    let save_path = if p.use_incomplete && !req.skip_check && incomplete != final_path {
        meta.target = Some(final_path.to_string_lossy().to_string());
        incomplete
    } else {
        final_path
    };
    let trackers: Vec<String> = p.extra_trackers.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    let params = AddParams {
        magnet: req.magnet.clone(),
        torrent: req.torrent.clone(),
        resume: Vec::new(),
        save_path: save_path.to_string_lossy().to_string(),
        name: req.name.clone(),
        paused: req.paused,
        auto_managed: !req.paused,
        sequential: req.sequential,
        first_last: req.first_last,
        skip_check: req.skip_check,
        preallocate: p.preallocate,
        disable_pex: !p.pex,
        content_layout: match req.layout.as_str() {
            "subfolder" => 1,
            "none" => 2,
            _ => 0,
        },
        file_priorities: req.priorities.clone(),
        trackers,
        dl_limit: 0,
        ul_limit: 0,
    };
    store::set_meta(&id, meta);
    live::send(Command::Add(Box::new(params), id.clone()));
    // Delete the source .torrent file if asked to (the data is already read).
    let doomed = DELETE_AFTER.with(|d| {
        let mut d = d.borrow_mut();
        let pos = d.iter().position(|(i, _)| *i == id);
        pos.map(|i| d.remove(i).1)
    });
    if let Some(path) = doomed {
        let _ = std::fs::remove_file(path);
    }
}

pub fn add_magnet_dialog(prefill: &str) {
    let (dialog, card) = widgets::dialog("Add magnet links", 560);
    card.append(&widgets::label("One link per line: magnet links, links to .torrent files, or info-hashes.", "dim"));
    let view = gtk::TextView::new();
    view.add_css_class("code-block");
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_size_request(-1, 120);
    view.buffer().set_text(prefill);
    // Paste a link from the clipboard if there's one there.
    if prefill.is_empty()
        && let Some(display) = gdk::Display::default()
    {
        let buffer = view.buffer();
        display.clipboard().read_text_async(gio::Cancellable::NONE, move |res| {
            if let Ok(Some(text)) = res
                && (text.starts_with("magnet:") || text.starts_with("http"))
                && buffer.char_count() == 0
            {
                buffer.set_text(&text);
            }
        });
    }
    card.append(&view);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Add");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        let buf = view.buffer();
        let text = buf.text(&buf.start_iter(), &buf.end_iter(), false).to_string();
        d.close();
        for line in text.lines() {
            add_link(line);
        }
    });
    dialog.present();
}

// ---------- Removing ----------

pub fn remove_dialog(ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let title = if ids.len() == 1 { "Remove this torrent?".to_string() } else { format!("Remove {} torrents?", ids.len()) };
    let (dialog, card) = widgets::dialog(&title, 480);
    let names: Vec<String> = ids.iter().take(5).map(|id| name_of(id)).collect();
    let mut text = names.join("\n");
    if ids.len() > 5 {
        text.push_str(&format!("\n…and {} more", ids.len() - 5));
    }
    let list = widgets::label(&text, "dim");
    list.set_wrap(true);
    card.append(&list);
    let (row, sw) = widgets::switch_row("Also delete the downloaded files", "They can't be recovered afterwards.", false, |_| {});
    card.append(&row);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Remove");
    ok.add_css_class("destructive-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        live::send(Command::Remove { ids: ids.clone(), with_files: sw.is_active() });
        for id in &ids {
            store::forget(id);
        }
        d.close();
    });
    dialog.present();
    cancel.grab_focus();
}

pub fn toggle_pause(ids: &[String]) {
    let all_paused = ids.iter().all(|id| transfers::row(id).is_some_and(|r| model::stopped(&r.s)));
    if all_paused {
        live::send(Command::Resume { ids: ids.to_vec(), force: false });
    } else {
        live::send(Command::Pause(ids.to_vec()));
    }
}

// ---------- Other torrent actions ----------

fn content_path(id: &str) -> Option<PathBuf> {
    let r = transfers::row(id)?;
    Some(Path::new(&r.s.save_path).join(&r.s.name))
}

pub fn open_folder(id: &str) {
    let Some(r) = transfers::row(id) else { return };
    let content = Path::new(&r.s.save_path).join(&r.s.name);
    let target = if content.is_dir() { content } else { PathBuf::from(&r.s.save_path) };
    if !target.exists() {
        window::toast("The download folder doesn't exist yet.");
        return;
    }
    cmd::spawn(&["xdg-open", &target.to_string_lossy()]);
}

/// Open what a torrent downloaded: its folder, or its one file in the default app.
pub fn open_content(id: &str) {
    let Some(content) = content_path(id) else { return };
    if !content.exists() {
        window::toast("The downloaded files aren't there any more.");
        return;
    }
    cmd::spawn(&["xdg-open", &content.to_string_lossy()]);
}

fn magnet_for(id: &str) -> String {
    if let Some(d) = live::latest().and_then(|u| u.details.clone()).filter(|d| d.id == id)
        && !d.magnet.is_empty()
    {
        return d.magnet;
    }
    let name = transfers::row(id).map(|r| r.s.name).unwrap_or_default();
    let dn: String = glib::Uri::escape_string(&name, None, false).into();
    format!("magnet:?xt=urn:btih:{id}&dn={dn}")
}

fn copy(text: &str) {
    if let Some(display) = gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

pub fn copy_magnets(ids: &[String]) {
    let text: Vec<String> = ids.iter().map(|id| magnet_for(id)).collect();
    copy(&text.join("\n"));
    window::toast(if ids.len() == 1 { "Magnet link copied." } else { "Magnet links copied." });
}

fn entry_dialog(title: &str, note: &str, value: &str, mono: bool, ok_label: &str, done: impl Fn(String) + 'static) {
    let (dialog, card) = widgets::dialog(title, 480);
    if !note.is_empty() {
        let l = widgets::label(note, "dim");
        l.set_wrap(true);
        card.append(&l);
    }
    let entry = gtk::Entry::new();
    entry.set_text(value);
    if mono {
        entry.add_css_class("mono");
    }
    card.append(&entry);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label(ok_label);
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let done = std::rc::Rc::new(done);
    let (d, e) = (dialog.clone(), entry.clone());
    let f = done.clone();
    ok.connect_clicked(move |_| {
        f(e.text().trim().to_string());
        d.close();
    });
    let d = dialog.clone();
    entry.connect_activate(move |e| {
        done(e.text().trim().to_string());
        d.close();
    });
    dialog.present();
    entry.grab_focus();
}

/// A folder field with a Browse… button.
pub fn folder_field(value: &str) -> (gtk::Box, gtk::Entry) {
    let bx = widgets::hbox(6);
    let entry = gtk::Entry::new();
    entry.set_text(value);
    entry.add_css_class("mono");
    entry.set_hexpand(true);
    let browse = gtk::Button::from_icon_name("folder-open-symbolic");
    browse.set_tooltip_text(Some("Choose a folder"));
    let e = entry.clone();
    browse.connect_clicked(move |b| {
        let dialog = gtk::FileDialog::builder().title("Choose a folder").modal(true).build();
        let start = paths::expand(&e.text());
        if start.is_dir() {
            dialog.set_initial_folder(Some(&gio::File::for_path(&start)));
        }
        let parent = b.root().and_downcast::<gtk::Window>();
        let e = e.clone();
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res
                && let Some(p) = f.path()
            {
                e.set_text(&paths::pretty(&p));
            }
        });
    });
    bx.append(&entry);
    bx.append(&browse);
    (bx, entry)
}

fn set_location_dialog(ids: Vec<String>) {
    let current = ids.first().and_then(|id| transfers::row(id)).map(|r| r.s.save_path).unwrap_or_default();
    let (dialog, card) = widgets::dialog("Move to another folder", 520);
    card.append(&widgets::label("The downloaded files move with it.", "dim"));
    let (field, entry) = folder_field(&paths::pretty(Path::new(&current)));
    card.append(&field);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Move");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        let path = paths::expand(&entry.text());
        let _ = std::fs::create_dir_all(&path);
        store::update_meta(&ids, |m| m.target = None);
        live::send(Command::Move(ids.clone(), path.to_string_lossy().to_string()));
        d.close();
    });
    dialog.present();
}

fn limits_dialog(ids: Vec<String>) {
    let first = ids.first().and_then(|id| transfers::row(id));
    let (dl, ul) = first.map(|r| (r.s.dl_limit.max(0) / 1024, r.s.ul_limit.max(0) / 1024)).unwrap_or((0, 0));
    let (dialog, card) = widgets::dialog("Limit speed", 440);
    card.append(&widgets::label("KiB/s for the selected torrents. 0 means no limit of their own.", "dim"));
    let spin = |v: i32| {
        let s = gtk::SpinButton::with_range(0.0, 1_000_000.0, 64.0);
        s.set_value(v as f64);
        s.add_css_class("tnum");
        s
    };
    let down = spin(dl);
    let up = spin(ul);
    card.append(&widgets::row("Download", "", Some(down.upcast_ref())));
    card.append(&widgets::row("Upload", "", Some(up.upcast_ref())));
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Apply");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let d = dialog.clone();
    ok.connect_clicked(move |_| {
        live::send(Command::Limits(ids.clone(), down.value() as i32 * 1024, up.value() as i32 * 1024));
        d.close();
    });
    dialog.present();
}

fn export_dialog(ids: Vec<String>) {
    let dialog = gtk::FileDialog::builder().title("Export .torrent").modal(true).build();
    let parent = window::window();
    if ids.len() == 1 {
        dialog.set_initial_name(Some(&format!("{}.torrent", name_of(&ids[0]))));
        dialog.save(parent.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res
                && let Some(p) = f.path()
            {
                live::send(Command::Export(ids[0].clone(), p));
            }
        });
    } else {
        dialog.select_folder(parent.as_ref(), gio::Cancellable::NONE, move |res| {
            if let Ok(f) = res
                && let Some(dir) = f.path()
            {
                for id in &ids {
                    let file = name_of(id).replace('/', "_");
                    live::send(Command::Export(id.clone(), dir.join(format!("{file}.torrent"))));
                }
            }
        });
    }
}

// ---------- Context menu ----------

fn action(group: &gio::SimpleActionGroup, name: &str, f: impl Fn() + 'static) {
    let a = gio::SimpleAction::new(name, None);
    a.connect_activate(move |_, _| f());
    group.add_action(&a);
}

fn toggle_action(group: &gio::SimpleActionGroup, name: &str, state: bool, f: impl Fn(bool) + 'static) {
    let a = gio::SimpleAction::new_stateful(name, None, &state.to_variant());
    a.connect_activate(move |a, _| {
        let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
        a.set_state(&on.to_variant());
        f(on);
    });
    group.add_action(&a);
}

pub fn context_menu(widget: &impl IsA<gtk::Widget>, x: f64, y: f64, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let rows: Vec<_> = ids.iter().filter_map(|id| transfers::row(id)).collect();
    let all = |f: &dyn Fn(&model::Row) -> bool| !rows.is_empty() && rows.iter().all(f);
    let group = gio::SimpleActionGroup::new();
    let menu = gio::Menu::new();

    let s = gio::Menu::new();
    let i = ids.clone();
    action(&group, "resume", move || live::send(Command::Resume { ids: i.clone(), force: false }));
    let i = ids.clone();
    action(&group, "force", move || live::send(Command::Resume { ids: i.clone(), force: true }));
    let i = ids.clone();
    action(&group, "pause", move || live::send(Command::Pause(i.clone())));
    if !all(&|r| !model::stopped(&r.s)) {
        s.append(Some("Resume"), Some("tr.resume"));
    }
    s.append(Some("Force resume"), Some("tr.force"));
    if !all(&|r| model::stopped(&r.s)) {
        s.append(Some("Pause"), Some("tr.pause"));
    }
    menu.append_section(None, &s);

    let s = gio::Menu::new();
    let i = ids.clone();
    action(&group, "remove", move || remove_dialog(i.clone()));
    s.append(Some("Remove…"), Some("tr.remove"));
    menu.append_section(None, &s);

    let s = gio::Menu::new();
    let i = ids.clone();
    action(&group, "location", move || set_location_dialog(i.clone()));
    s.append(Some("Move to…"), Some("tr.location"));
    if ids.len() == 1 {
        let id = ids[0].clone();
        action(&group, "rename", move || {
            let id = id.clone();
            entry_dialog(
                "Rename",
                "The torrent's top folder (or its only file) is renamed on disk.",
                &name_of(&id),
                false,
                "Rename",
                move |name| {
                    if !name.is_empty() {
                        live::send(Command::Rename(id.clone(), name));
                    }
                },
            );
        });
        s.append(Some("Rename…"), Some("tr.rename"));
    }
    let i = ids.clone();
    action(&group, "limits", move || limits_dialog(i.clone()));
    s.append(Some("Limit speed…"), Some("tr.limits"));

    // Category
    let cats = gio::Menu::new();
    let i = ids.clone();
    action(&group, "cat-new", move || {
        let i = i.clone();
        edit_category_then(None, move |name| {
            store::update_meta(&i, |m| m.category = name.clone());
            transfers::refresh_meta();
        })
    });
    let i = ids.clone();
    action(&group, "cat-none", move || {
        store::update_meta(&i, |m| m.category.clear());
        transfers::refresh_meta();
    });
    let current_cat = rows.first().map(|r| r.meta.category.clone()).unwrap_or_default();
    let cat_sub = gio::Menu::new();
    cat_sub.append(Some("New category…"), Some("tr.cat-new"));
    cat_sub.append(Some("None"), Some("tr.cat-none"));
    for (n, name) in store::categories().keys().enumerate() {
        let act = format!("cat-{n}");
        let (i, name2) = (ids.clone(), name.clone());
        action(&group, &act, move || {
            store::update_meta(&i, |m| m.category = name2.clone());
            // Follow the category's folder, like qBittorrent's automatic mode.
            let path = store::save_path(&name2);
            let _ = std::fs::create_dir_all(&path);
            live::send(Command::Move(i.clone(), path.to_string_lossy().to_string()));
            transfers::refresh_meta();
        });
        let label =
            if *name == current_cat && all(&|r| r.meta.category == current_cat) { format!("✓ {name}") } else { name.clone() };
        cats.append(Some(&label), Some(&format!("tr.{act}")));
    }
    cat_sub.append_section(None, &cats);
    s.append_submenu(Some("Category"), &cat_sub);

    // Tags
    let tag_sub = gio::Menu::new();
    let i = ids.clone();
    action(&group, "tag-new", move || {
        let i = i.clone();
        entry_dialog("New tag", "", "", false, "Add", move |name| {
            if !name.is_empty() {
                store::add_tag(&name);
                store::update_meta(&i, |m| {
                    if !m.tags.contains(&name) {
                        m.tags.push(name.clone());
                    }
                });
                transfers::refresh_meta();
            }
        })
    });
    let i = ids.clone();
    action(&group, "tag-clear", move || {
        store::update_meta(&i, |m| m.tags.clear());
        transfers::refresh_meta();
    });
    tag_sub.append(Some("New tag…"), Some("tr.tag-new"));
    tag_sub.append(Some("Remove all tags"), Some("tr.tag-clear"));
    let tags = gio::Menu::new();
    for (n, tag) in store::tags().iter().enumerate() {
        let act = format!("tag-{n}");
        let has = all(&|r| r.meta.tags.contains(tag));
        let (i, t) = (ids.clone(), tag.clone());
        toggle_action(&group, &act, has, move |on| {
            store::update_meta(&i, |m| {
                m.tags.retain(|x| *x != t);
                if on {
                    m.tags.push(t.clone());
                }
            });
            transfers::refresh_meta();
        });
        tags.append(Some(tag), Some(&format!("tr.{act}")));
    }
    tag_sub.append_section(None, &tags);
    s.append_submenu(Some("Tags"), &tag_sub);
    menu.append_section(None, &s);

    // Queue
    let s = gio::Menu::new();
    let queue = gio::Menu::new();
    for (name, label, how) in [
        ("q-up", "Move up", Queue::Up),
        ("q-down", "Move down", Queue::Down),
        ("q-top", "Move to top", Queue::Top),
        ("q-bottom", "Move to bottom", Queue::Bottom),
    ] {
        let i = ids.clone();
        action(&group, name, move || live::send(Command::Queue(i.clone(), how)));
        queue.append(Some(label), Some(&format!("tr.{name}")));
    }
    s.append_submenu(Some("Queue"), &queue);
    let i = ids.clone();
    action(&group, "recheck", move || live::send(Command::Recheck(i.clone())));
    s.append(Some("Force recheck"), Some("tr.recheck"));
    let i = ids.clone();
    action(&group, "reannounce", move || live::send(Command::Reannounce(i.clone())));
    s.append(Some("Force reannounce"), Some("tr.reannounce"));
    menu.append_section(None, &s);

    // Download order
    let s = gio::Menu::new();
    let i = ids.clone();
    toggle_action(&group, "sequential", all(&|r| r.s.sequential), move |on| live::send(Command::Sequential(i.clone(), on)));
    s.append(Some("Download in order"), Some("tr.sequential"));
    let i = ids.clone();
    toggle_action(&group, "firstlast", all(&|r| r.meta.first_last), move |on| {
        store::update_meta(&i, |m| m.first_last = on);
        live::send(Command::FirstLast(i.clone(), on));
    });
    s.append(Some("First and last pieces first"), Some("tr.firstlast"));
    if all(&|r| model::done(&r.s)) {
        let i = ids.clone();
        toggle_action(&group, "superseed", all(&|r| r.s.super_seeding), move |on| live::send(Command::SuperSeed(i.clone(), on)));
        s.append(Some("Super seeding"), Some("tr.superseed"));
    }
    menu.append_section(None, &s);

    // Copy, open, export
    let s = gio::Menu::new();
    let copy_menu = gio::Menu::new();
    let i = ids.clone();
    action(&group, "copy-name", move || {
        copy(&i.iter().map(|id| name_of(id)).collect::<Vec<_>>().join("\n"));
    });
    let i = ids.clone();
    action(&group, "copy-hash", move || copy(&i.join("\n")));
    let i = ids.clone();
    action(&group, "copy-magnet", move || copy_magnets(&i));
    copy_menu.append(Some("Name"), Some("tr.copy-name"));
    copy_menu.append(Some("Info hash"), Some("tr.copy-hash"));
    copy_menu.append(Some("Magnet link"), Some("tr.copy-magnet"));
    s.append_submenu(Some("Copy"), &copy_menu);
    if ids.len() == 1 {
        let id = ids[0].clone();
        action(&group, "open", move || open_folder(&id));
        s.append(Some("Open folder"), Some("tr.open"));
        if let Some(path) = content_path(&ids[0])
            && path.is_file()
        {
            action(&group, "open-file", move || cmd::spawn(&["xdg-open", &path.to_string_lossy()]));
            s.append(Some("Open file"), Some("tr.open-file"));
        }
    }
    let i = ids.clone();
    action(&group, "export", move || export_dialog(i.clone()));
    s.append(Some("Export .torrent…"), Some("tr.export"));
    menu.append_section(None, &s);

    popup(widget, x, y, &menu, &group);
}

pub fn popup(widget: &impl IsA<gtk::Widget>, x: f64, y: f64, menu: &gio::Menu, group: &gio::SimpleActionGroup) {
    let widget = widget.upcast_ref::<gtk::Widget>();
    widget.insert_action_group("tr", Some(group));
    let pop = gtk::PopoverMenu::from_model(Some(menu));
    pop.set_parent(widget);
    pop.set_has_arrow(false);
    pop.set_halign(gtk::Align::Start);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    pop.connect_closed(|p| {
        let p = p.clone();
        // Unparent after the activated action has run.
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

pub fn tracker_menu(widget: &impl IsA<gtk::Widget>, x: f64, y: f64, url: &str) {
    let Some(id) = transfers::selection().first().cloned() else { return };
    let group = gio::SimpleActionGroup::new();
    let menu = gio::Menu::new();
    let u = url.to_string();
    action(&group, "copy", move || copy(&u));
    let u = url.to_string();
    action(&group, "remove", move || live::send(Command::RemoveTracker(id.clone(), u.clone())));
    menu.append(Some("Copy URL"), Some("tr.copy"));
    menu.append(Some("Remove tracker"), Some("tr.remove"));
    popup(widget, x, y, &menu, &group);
}

// ---------- Categories and tags in the sidebar ----------

fn ids_matching(f: &Filter) -> Vec<String> {
    live::latest()
        .map(|u| u.torrents.iter().filter(|s| f.matches(s, &store::meta(&s.id))).map(|s| s.id.clone()).collect())
        .unwrap_or_default()
}

/// Right-click a category or tag in the sidebar: edit, remove, pause or resume all.
pub fn attach_list_menu(button: &gtk::Button, filter: &Filter) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_SECONDARY);
    let f = filter.clone();
    let b = button.clone();
    click.connect_pressed(move |_, _, x, y| {
        let group = gio::SimpleActionGroup::new();
        let menu = gio::Menu::new();
        let top = gio::Menu::new();
        match &f {
            Filter::Category(Some(name)) => {
                let n = name.clone();
                action(&group, "edit", move || edit_category_dialog(Some(n.clone())));
                top.append(Some("Edit category…"), Some("tr.edit"));
                let n = name.clone();
                action(&group, "remove", move || {
                    store::remove_category(&n);
                    transfers::refresh_meta();
                });
                top.append(Some("Remove category"), Some("tr.remove"));
            }
            Filter::Tag(Some(name)) => {
                let n = name.clone();
                action(&group, "remove", move || {
                    store::remove_tag(&n);
                    transfers::refresh_meta();
                });
                top.append(Some("Remove tag"), Some("tr.remove"));
            }
            _ => {}
        }
        menu.append_section(None, &top);
        let s = gio::Menu::new();
        let f2 = f.clone();
        action(&group, "resume", move || live::send(Command::Resume { ids: ids_matching(&f2), force: false }));
        let f2 = f.clone();
        action(&group, "pause", move || live::send(Command::Pause(ids_matching(&f2))));
        s.append(Some("Resume all"), Some("tr.resume"));
        s.append(Some("Pause all"), Some("tr.pause"));
        menu.append_section(None, &s);
        popup(&b, x, y, &menu, &group);
    });
    button.add_controller(click);
}

pub fn edit_category_dialog(name: Option<String>) {
    edit_category_then(name, |_| {});
}

fn edit_category_then(name: Option<String>, then: impl Fn(String) + 'static) {
    let existing = name.as_ref().and_then(|n| store::categories().get(n).cloned()).unwrap_or_default();
    let (dialog, card) = widgets::dialog(if name.is_some() { "Edit category" } else { "New category" }, 520);
    let name_entry = gtk::Entry::new();
    name_entry.set_placeholder_text(Some("Name, e.g. Linux or Movies/HD"));
    name_entry.set_text(name.as_deref().unwrap_or(""));
    card.append(&widgets::label("Name", "kv-key"));
    card.append(&name_entry);
    card.append(&widgets::label("Save to", "kv-key"));
    let (field, path_entry) = folder_field(&existing.save_path);
    path_entry.set_placeholder_text(Some("Default folder"));
    card.append(&field);
    let note = widgets::label(
        "Leave empty to save in the default folder (in a subfolder named after the category, if that's turned on in Downloads).",
        "dim",
    );
    note.set_wrap(true);
    card.append(&note);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label("Save");
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    let d = dialog.clone();
    cancel.connect_clicked(move |_| d.close());
    let (d, focus) = (dialog.clone(), name_entry.clone());
    ok.connect_clicked(move |_| {
        let new = name_entry.text().trim().trim_matches('/').to_string();
        if new.is_empty() {
            name_entry.add_css_class("error");
            return;
        }
        let cat = Category { save_path: path_entry.text().trim().to_string() };
        if let Some(old) = &name
            && *old != new
        {
            let ids = ids_matching(&Filter::Category(Some(old.clone())));
            store::update_meta(&ids, |m| {
                if m.category == *old {
                    m.category = new.clone();
                }
            });
            store::remove_category(old);
            store::update_meta(&ids, |m| {
                if m.category.is_empty() {
                    m.category = new.clone();
                }
            });
        }
        store::set_category(&new, cat);
        transfers::refresh_meta();
        then(new);
        d.close();
    });
    dialog.present();
    focus.grab_focus();
}

pub fn new_tag_dialog() {
    entry_dialog("New tag", "Tags are labels you can give any number of torrents.", "", false, "Add", |name| {
        if !name.is_empty() {
            store::add_tag(&name);
        }
    });
}

// ---------- Speed ----------

pub fn toggle_alt_speed() {
    let on = !live::alt_active();
    let was_scheduled = prefs::get().schedule;
    prefs::update(|p| {
        p.alt_enabled = on;
        p.schedule = false;
    });
    live::apply_settings_now();
    window::refresh_alt_speed();
    let p = prefs::get();
    let msg = if on {
        format!(
            "Alternative speed limits on: {} down, {} up.",
            window::limit_text(p.alt_dl_limit),
            window::limit_text(p.alt_ul_limit)
        )
    } else {
        "Alternative speed limits off.".to_string()
    };
    window::toast(&if was_scheduled { format!("{msg} The schedule is turned off.") } else { msg });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("a b"), "'a b'");
    }
}
