//! Torrents as the UI sees them: libtorrent's status plus our own metadata
//! (category, tags), how to describe their state, and which sidebar filters
//! they belong to.

use crate::engine::Status;
use crate::store::Meta;
use gtk::glib;
use gtk::subclass::prelude::*;
use std::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Downloading,
    Stalled,
    Metadata,
    Seeding,
    Idle,
    Paused,
    Completed,
    Queued,
    QueuedSeed,
    Checking,
    Moving,
    Error,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Downloading => "Downloading",
            Kind::Stalled => "Stalled",
            Kind::Metadata => "Fetching metadata",
            Kind::Seeding => "Seeding",
            Kind::Idle => "Seeding (idle)",
            Kind::Paused => "Paused",
            Kind::Completed => "Completed",
            Kind::Queued => "Queued",
            Kind::QueuedSeed => "Queued to seed",
            Kind::Checking => "Checking",
            Kind::Moving => "Moving",
            Kind::Error => "Error",
        }
    }

    /// A symbolic icon for the name column and the details header.
    pub fn icon(self) -> &'static str {
        match self {
            Kind::Downloading | Kind::Metadata => "go-down-symbolic",
            Kind::Stalled => "network-idle-symbolic",
            Kind::Seeding => "go-up-symbolic",
            Kind::Idle => "emblem-ok-symbolic",
            Kind::Paused => "media-playback-pause-symbolic",
            Kind::Completed => "emblem-ok-symbolic",
            Kind::Queued | Kind::QueuedSeed => "view-list-symbolic",
            Kind::Checking | Kind::Moving => "view-refresh-symbolic",
            Kind::Error => "dialog-warning-symbolic",
        }
    }
}

/// libtorrent's `torrent_status::state_t` values we care about.
const CHECKING_FILES: u8 = 1;
const DOWNLOADING_METADATA: u8 = 2;
const CHECKING_RESUME: u8 = 7;

pub fn done(s: &Status) -> bool {
    s.has_metadata && (s.is_finished || s.is_seeding)
}

/// Paused by the user (not just waiting in the queue).
pub fn stopped(s: &Status) -> bool {
    s.paused && !s.auto_managed
}

/// Running outside the queue's limits ("Force resume").
pub fn forced(s: &Status) -> bool {
    !s.paused && !s.auto_managed
}

pub fn kind(s: &Status) -> Kind {
    if !s.error.is_empty() {
        return Kind::Error;
    }
    if s.moving {
        return Kind::Moving;
    }
    if s.state == CHECKING_FILES || s.state == CHECKING_RESUME {
        return Kind::Checking;
    }
    let done = done(s);
    if stopped(s) {
        return if done { Kind::Completed } else { Kind::Paused };
    }
    if s.paused {
        return if done { Kind::QueuedSeed } else { Kind::Queued };
    }
    if s.state == DOWNLOADING_METADATA {
        return Kind::Metadata;
    }
    match (done, s.dl_rate > 0, s.ul_rate > 0) {
        (true, _, true) => Kind::Seeding,
        (true, _, false) => Kind::Idle,
        (false, true, _) => Kind::Downloading,
        (false, false, _) => Kind::Stalled,
    }
}

pub fn status_text(s: &Status) -> String {
    let k = kind(s);
    if forced(s) && matches!(k, Kind::Downloading | Kind::Stalled | Kind::Seeding | Kind::Idle) {
        format!("{} (forced)", k.label())
    } else {
        k.label().to_string()
    }
}

/// Seconds until done, if it's downloading at all.
pub fn eta(s: &Status) -> Option<i64> {
    if done(s) || s.dl_rate <= 0 || stopped(s) {
        return None;
    }
    Some(((s.wanted - s.wanted_done).max(0) / s.dl_rate.max(1)).max(0))
}

/// Upload ratio. Torrents that were seeded from local files count against their size.
pub fn ratio(s: &Status) -> f64 {
    let base = s.downloaded.max(if s.downloaded == 0 { s.total_done } else { 0 });
    if base <= 0 {
        return if s.uploaded > 0 { f64::INFINITY } else { 0.0 };
    }
    s.uploaded as f64 / base as f64
}

// ---------- Sidebar filters ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    All,
    Downloading,
    Seeding,
    Completed,
    Paused,
    Active,
    Inactive,
    Errored,
    /// `None` is "Uncategorized".
    Category(Option<String>),
    /// `None` is "Untagged".
    Tag(Option<String>),
}

pub const STATUS_FILTERS: &[(&str, &str, &str)] = &[
    ("all", "All", "view-list-symbolic"),
    ("downloading", "Downloading", "go-down-symbolic"),
    ("seeding", "Seeding", "go-up-symbolic"),
    ("completed", "Finished", "emblem-ok-symbolic"),
    ("paused", "Paused", "media-playback-pause-symbolic"),
    ("active", "Active", "network-transmit-receive-symbolic"),
    ("inactive", "Inactive", "network-idle-symbolic"),
    ("errored", "Problems", "dialog-warning-symbolic"),
];

impl Filter {
    pub fn from_id(id: &str) -> Option<Filter> {
        if let Some(c) = id.strip_prefix("cat:") {
            return Some(Filter::Category((!c.is_empty()).then(|| c.to_string())));
        }
        if let Some(t) = id.strip_prefix("tag:") {
            return Some(Filter::Tag((!t.is_empty()).then(|| t.to_string())));
        }
        Some(match id {
            "all" => Filter::All,
            "downloading" => Filter::Downloading,
            "seeding" => Filter::Seeding,
            "completed" => Filter::Completed,
            "paused" => Filter::Paused,
            "active" => Filter::Active,
            "inactive" => Filter::Inactive,
            "errored" => Filter::Errored,
            _ => return None,
        })
    }

    pub fn id(&self) -> String {
        match self {
            Filter::All => "all".into(),
            Filter::Downloading => "downloading".into(),
            Filter::Seeding => "seeding".into(),
            Filter::Completed => "completed".into(),
            Filter::Paused => "paused".into(),
            Filter::Active => "active".into(),
            Filter::Inactive => "inactive".into(),
            Filter::Errored => "errored".into(),
            Filter::Category(c) => format!("cat:{}", c.as_deref().unwrap_or("")),
            Filter::Tag(t) => format!("tag:{}", t.as_deref().unwrap_or("")),
        }
    }

    pub fn title(&self) -> String {
        match self {
            Filter::Category(None) => "Uncategorized".into(),
            Filter::Category(Some(c)) => c.clone(),
            Filter::Tag(None) => "Untagged".into(),
            Filter::Tag(Some(t)) => t.clone(),
            other => STATUS_FILTERS.iter().find(|f| f.0 == other.id()).map(|f| f.1.to_string()).unwrap_or_default(),
        }
    }

    pub fn matches(&self, s: &Status, m: &Meta) -> bool {
        let active = s.dl_rate > 0 || s.ul_rate > 0;
        match self {
            Filter::All => true,
            Filter::Downloading => !done(s) && !stopped(s) && s.error.is_empty(),
            Filter::Seeding => done(s) && !stopped(s) && s.error.is_empty(),
            Filter::Completed => done(s),
            Filter::Paused => stopped(s),
            Filter::Active => active,
            Filter::Inactive => !active,
            Filter::Errored => !s.error.is_empty(),
            Filter::Category(None) => m.category.is_empty(),
            Filter::Category(Some(c)) => m.category == *c || m.category.starts_with(&format!("{c}/")),
            Filter::Tag(None) => m.tags.is_empty(),
            Filter::Tag(Some(t)) => m.tags.iter().any(|x| x == t),
        }
    }
}

// ---------- The row object ----------

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub s: Status,
    pub meta: Meta,
    pub visible: bool,
    pub order: u32,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TorrentObject {
        pub row: RefCell<Row>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TorrentObject {
        const NAME: &'static str = "TorrentsTorrentObject";
        type Type = super::TorrentObject;
    }

    impl ObjectImpl for TorrentObject {}
}

glib::wrapper! {
    pub struct TorrentObject(ObjectSubclass<imp::TorrentObject>);
}

impl TorrentObject {
    pub fn new(row: Row) -> Self {
        let o: Self = glib::Object::new();
        *o.imp().row.borrow_mut() = row;
        o
    }
    pub fn row(&self) -> std::cell::Ref<'_, Row> {
        self.imp().row.borrow()
    }
    pub fn row_mut(&self) -> std::cell::RefMut<'_, Row> {
        self.imp().row.borrow_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> Status {
        Status { has_metadata: true, auto_managed: true, state: 3, ..Default::default() }
    }

    #[test]
    fn classifies_states() {
        let mut s = st();
        assert_eq!(kind(&s), Kind::Stalled);
        s.dl_rate = 10;
        assert_eq!(kind(&s), Kind::Downloading);
        s.paused = true;
        assert_eq!(kind(&s), Kind::Queued);
        s.auto_managed = false;
        assert_eq!(kind(&s), Kind::Paused);
        s.is_finished = true;
        assert_eq!(kind(&s), Kind::Completed);
        s.paused = false;
        s.ul_rate = 5;
        assert_eq!(kind(&s), Kind::Seeding);
        assert_eq!(status_text(&s), "Seeding (forced)");
        s.error = "disk full".into();
        assert_eq!(kind(&s), Kind::Error);
    }

    #[test]
    fn filters() {
        let meta = Meta { category: "Linux/ISOs".into(), tags: vec!["arch".into()], ..Default::default() };
        let mut s = st();
        assert!(Filter::Downloading.matches(&s, &meta));
        assert!(!Filter::Seeding.matches(&s, &meta));
        assert!(Filter::Category(Some("Linux".into())).matches(&s, &meta));
        assert!(!Filter::Category(None).matches(&s, &meta));
        assert!(Filter::Tag(Some("arch".into())).matches(&s, &meta));
        s.is_seeding = true;
        assert!(Filter::Seeding.matches(&s, &meta));
        assert!(Filter::Completed.matches(&s, &meta));
        assert!(Filter::Inactive.matches(&s, &meta));
    }

    #[test]
    fn filter_ids_round_trip() {
        for id in ["all", "errored", "cat:", "cat:Movies", "tag:", "tag:hd"] {
            assert_eq!(Filter::from_id(id).unwrap().id(), id);
        }
        assert!(Filter::from_id("search").is_none());
    }

    #[test]
    fn ratio_and_eta() {
        let mut s = st();
        s.downloaded = 100;
        s.uploaded = 250;
        assert_eq!(ratio(&s), 2.5);
        s.downloaded = 0;
        s.total_done = 500;
        assert_eq!(ratio(&s), 0.5);
        s.wanted = 1000;
        s.wanted_done = 400;
        s.dl_rate = 100;
        assert_eq!(eta(&s), Some(6));
    }
}
