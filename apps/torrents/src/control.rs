//! Torrents' tools on the control interface (`nyx_core::control`), for Nyx Assistant:
//! what's downloading, add a magnet or .torrent link, pause or resume one. Nothing is
//! removed; adding goes through the usual add dialog when that's switched on.

use crate::engine::{Command, ffi::Status};
use crate::{actions, live, model};
use nyx_core::control::text;
use serde_json::{Value, json};

/// Torrents' description for the control interface (API 2): its actions, the `nyx`
/// command's `torrents` namespace, and D-Bus activation so a call starts it (with its window,
/// which holds what the actions work on).
pub fn description(app_id: &str) -> Value {
    let mut tools = tools();
    for t in tools.as_array_mut().into_iter().flatten() {
        let risk = match t["name"].as_str().unwrap_or_default() {
            "torrents.list" => "read_only",
            "torrents.add" => "external",
            _ => "writes",
        };
        t["x-nyx"] = json!({"risk": risk});
    }
    json!({
        "tools": tools,
        "nyx": {"api": 2, "app": app_id, "name": "Torrents", "icon": "io.github.design_nexus.nyx.Torrents", "namespaces": ["torrents"],
                "aliases": [], "bus_name": app_id, "activation": "dbus", "default": "torrents.list"}
    })
}

pub fn tools() -> Value {
    json!([
        {"name": "torrents.list", "description": "The torrents in Torrents: name, progress, state, speeds, time left.", "inputSchema": {"type": "object", "properties": {}}},
        {"name": "torrents.add", "description": "Add a torrent from a magnet link, an http(s) link to a .torrent, or an info-hash.", "inputSchema": {"type": "object", "properties": {"link": {"type": "string"}}, "required": ["link"]}},
        {"name": "torrents.pause", "description": "Pause a torrent (by name, or all).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "all": {"type": "boolean"}}}},
        {"name": "torrents.resume", "description": "Resume a paused torrent (by name, or all).", "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "all": {"type": "boolean"}}}},
    ])
}

fn torrents() -> Result<Vec<Status>, String> {
    live::latest().map(|u| u.torrents.clone()).ok_or_else(|| "Torrents is still starting; try again in a moment".into())
}

fn item(s: &Status) -> Value {
    json!({
        "name": s.name, "progress_percent": (s.progress * 1000.0).round() / 10.0, "state": model::status_text(s),
        "download_kib_s": s.dl_rate / 1024, "upload_kib_s": s.ul_rate / 1024, "seconds_left": model::eta(s),
        "size_mib": s.size / (1024 * 1024),
    })
}

/// The torrents a pause or resume is about: all, or the ones whose name holds the words.
fn chosen(args: &Value) -> Result<Vec<Status>, String> {
    let all = torrents()?;
    if args["all"] == true {
        return Ok(all);
    }
    let name = text(args, "name").map_err(|_| "give a name, or all: true")?.to_lowercase();
    let found: Vec<Status> = all.into_iter().filter(|s| s.name.to_lowercase().contains(&name)).collect();
    match found.len() {
        0 => Err("no torrent of that name".into()),
        1 => Ok(found),
        _ => Err(format!("several match: {}", found.iter().take(5).map(|s| s.name.as_str()).collect::<Vec<_>>().join("; "))),
    }
}

pub fn handle(name: &str, args: Value) -> Result<Value, String> {
    match name {
        "torrents.list" => Ok(json!({"torrents": torrents()?.iter().map(item).collect::<Vec<_>>(), "untrusted_content": true})),
        "torrents.add" => {
            let link = text(&args, "link")?;
            let t = link.trim();
            let hex = t.len() == 40 && t.chars().all(|c| c.is_ascii_hexdigit());
            // Links only: a file path could reach outside the home folder.
            if !(t.starts_with("magnet:") || hex || t.starts_with("https://") || t.starts_with("http://")) {
                return Err("give a magnet link, an http(s) link to a .torrent, or an info-hash".into());
            }
            actions::add_link(t);
            Ok(json!({"adding": t, "note": "Torrents may show its add dialog for the person to confirm"}))
        }
        "torrents.pause" | "torrents.resume" => {
            let picked = chosen(&args)?;
            let ids: Vec<String> = picked.iter().map(|s| s.id.clone()).collect();
            if name == "torrents.pause" {
                live::send(Command::Pause(ids));
            } else {
                live::send(Command::Resume { ids, force: false });
            }
            Ok(json!({"torrents": picked.iter().map(|s| s.name.clone()).collect::<Vec<_>>()}))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}
