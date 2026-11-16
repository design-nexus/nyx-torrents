use crate::widgets::{self, Page};
use crate::{live, prefs};
use gtk::prelude::*;

fn set(change: impl FnOnce(&mut prefs::Prefs)) {
    prefs::update(change);
    live::apply_settings();
}

/// Network interfaces worth binding to (not loopback).
fn interfaces() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net")
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n != "lo").collect())
        .unwrap_or_default();
    names.sort();
    names
}

pub fn build(page: &Page) {
    let p = prefs::get();

    // ----- Listening -----
    let g = page.group("Listening");
    let (r, spin) = widgets::spin_row(
        "Port",
        "Other peers connect to you here. Forward it on your router if UPnP isn't available.",
        (1024.0, 65535.0),
        1.0,
        0,
        p.port as f64,
        "",
        |v| set(|p| p.port = v as u16),
    );
    let random = gtk::Button::with_label("Random");
    random.set_valign(gtk::Align::Center);
    random.connect_clicked(move |_| spin.set_value(prefs::random_port() as f64));
    r.append(&random);
    g.add(&r);
    let (r, _) = widgets::switch_row(
        "Open the port automatically",
        "Ask your router to forward the port with UPnP and NAT-PMP.",
        p.upnp,
        |on| set(|p| p.upnp = on),
    );
    g.add(&r);
    if firewall::managed() {
        g.add(&firewall::row(p.port));
    }
    g.add(&widgets::segmented_row(
        "Protocols",
        "µTP is gentler on your connection; plain TCP reaches some older clients.",
        &[("both", "TCP and µTP"), ("tcp", "TCP"), ("utp", "µTP")],
        &p.protocol,
        |v| set(|p| p.protocol = v),
    ));

    // ----- Interface -----
    let g = page.group("Network interface");
    let mut options = vec![("".to_string(), "Any interface".to_string())];
    let mut names = interfaces();
    if !p.interface.is_empty() && !names.contains(&p.interface) {
        names.push(p.interface.clone());
    }
    options.extend(names.into_iter().map(|n| (n.clone(), n)));
    let (r, _) = widgets::choice_row(
        "Bind to",
        "Pick your VPN's interface (<tt>wg0</tt>, <tt>tun0</tt>) and torrents stop whenever the VPN is down.",
        options,
        &p.interface,
        |v| set(|p| p.interface = v),
    );
    g.add(&r);

    // ----- Limits -----
    let g = page.group("Connections");
    let (r, _) = widgets::spin_row(
        "Most connections",
        "Across all torrents. Too many can overwhelm a home router.",
        (0.0, 10000.0),
        10.0,
        0,
        p.max_connections as f64,
        "",
        |v| set(|p| p.max_connections = v as i32),
    );
    g.add(&r);
    let (r, _) = widgets::spin_row(
        "Upload slots",
        "How many peers you upload to at once. 0 means no limit.",
        (0.0, 1000.0),
        1.0,
        0,
        p.max_upload_slots as f64,
        "",
        |v| set(|p| p.max_upload_slots = v as i32),
    );
    g.add(&r);

    // ----- Proxy -----
    let g = page.group("Proxy");
    let details = widgets::vbox(6);
    details.set_visible(p.proxy_type != "none");
    let (r, _) = widgets::entry_row("Host", "", &p.proxy_host, "proxy.example", true, |v| set(|p| p.proxy_host = v));
    details.append(&r);
    let (r, _) =
        widgets::spin_row("Port", "", (1.0, 65535.0), 1.0, 0, p.proxy_port as f64, "", |v| set(|p| p.proxy_port = v as u16));
    details.append(&r);
    let (r, _) = widgets::entry_row("User name", "Leave empty if the proxy doesn't need one.", &p.proxy_user, "", false, |v| {
        set(|p| p.proxy_user = v)
    });
    details.append(&r);
    let (r, e) = widgets::entry_row("Password", "", &p.proxy_password, "", false, |v| set(|p| p.proxy_password = v));
    e.set_visibility(false);
    e.set_input_purpose(gtk::InputPurpose::Password);
    details.append(&r);
    let (r, _) = widgets::switch_row(
        "Use it for peers too",
        "Send peer connections through the proxy, not just trackers.",
        p.proxy_peers,
        |on| set(|p| p.proxy_peers = on),
    );
    details.append(&r);
    let (r, _) = widgets::switch_row(
        "Look up names through it",
        "Let the proxy resolve tracker host names, so DNS doesn't leak.",
        p.proxy_hostnames,
        |on| set(|p| p.proxy_hostnames = on),
    );
    details.append(&r);
    let d = details.clone();
    let (r, _) = widgets::choice_row(
        "Proxy",
        "Route traffic through a SOCKS or HTTP proxy.",
        widgets::opts(&[("none", "None"), ("socks5", "SOCKS5"), ("socks4", "SOCKS4"), ("http", "HTTP")]),
        &p.proxy_type,
        move |v| {
            d.set_visible(v != "none");
            set(|p| p.proxy_type = v);
        },
    );
    g.add(&r);
    g.add(&details);
}

/// NyxOS's firewall drops incoming connections unless a port is allowed in the system's
/// overrides (`nyx-system apply`, which asks for an administrator and updates the system).
/// Downloads work without it; allowing the port lets other peers reach this one.
mod firewall {
    use crate::widgets;
    use gtk::{gio, glib, prelude::*};

    const OVERRIDES: &str = "/etc/nyxos/overrides/system.json";

    /// On NyxOS, with its System Manager to ask.
    pub fn managed() -> bool {
        std::path::Path::new(OVERRIDES).parent().is_some_and(|d| d.is_dir())
            && std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("nyx-system").is_file()))
    }

    /// Whether the overrides allow `port` in for both TCP and UDP.
    pub fn allows(overrides: &serde_json::Value, port: u16) -> bool {
        let has = |proto: &str| {
            overrides["firewall"][proto].as_array().is_some_and(|ports| ports.iter().any(|p| p.as_u64() == Some(port as u64)))
        };
        has("tcp") && has("udp")
    }

    /// The System Manager's changes that open or close `port` for TCP and UDP.
    pub fn changes(port: u16, open: bool) -> String {
        let one =
            |protocol: &str| serde_json::json!({"change": "firewall-port", "port": port, "protocol": protocol, "open": open});
        serde_json::Value::Array(vec![one("tcp"), one("udp")]).to_string()
    }

    fn allowed_now(port: u16) -> bool {
        std::fs::read_to_string(OVERRIDES)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .is_some_and(|v| allows(&v, port))
    }

    fn describe(port: u16, open: bool) -> String {
        if open {
            format!("NyxOS's firewall lets other peers reach port {port}.")
        } else {
            format!(
                "NyxOS's firewall turns away peers connecting to port {port}; downloads still work. Allowing it asks for an administrator and updates the system."
            )
        }
    }

    pub fn row(port: u16) -> gtk::Box {
        let open = allowed_now(port);
        let switch = gtk::Switch::new();
        switch.set_active(open);
        switch.set_valign(gtk::Align::Center);
        let row = widgets::row("Allow incoming connections", &describe(port, open), Some(switch.upcast_ref()));
        let desc = row_description(&row);
        switch.connect_state_set(move |switch, want| {
            if want == allowed_now(port) {
                return glib::Propagation::Proceed;
            }
            switch.set_sensitive(false);
            if let Some(d) = &desc {
                d.set_text("Updating the system…");
            }
            let json = changes(port, want);
            let args = ["nyx-system", "apply", json.as_str()].map(std::ffi::OsStr::new);
            let (switch, desc) = (switch.clone(), desc.clone());
            match gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE) {
                Ok(child) => child.wait_check_async(None::<&gio::Cancellable>, move |result| {
                    let now = allowed_now(port);
                    if let Err(e) = result {
                        crate::window::toast(&format!("The firewall wasn't changed: {}", e.message()));
                    }
                    switch.set_active(now);
                    switch.set_state(now);
                    switch.set_sensitive(true);
                    if let Some(d) = &desc {
                        d.set_text(&describe(port, now));
                    }
                }),
                Err(e) => {
                    crate::window::toast(&format!("The System Manager couldn't be asked: {}", e.message()));
                    switch.set_sensitive(true);
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Stop
        });
        row
    }

    /// The row's description label (the second label in its text column).
    fn row_description(row: &gtk::Box) -> Option<gtk::Label> {
        let mut labels = Vec::new();
        let mut stack = vec![row.clone().upcast::<gtk::Widget>()];
        while let Some(w) = stack.pop() {
            if let Ok(l) = w.clone().downcast::<gtk::Label>() {
                labels.push(l);
            }
            let mut child = w.last_child();
            while let Some(c) = child {
                stack.push(c.clone());
                child = c.prev_sibling();
            }
        }
        labels.into_iter().nth(1)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn both_protocols_must_be_open() {
            let v = serde_json::json!({"firewall": {"tcp": [22, 51413], "udp": [51413]}});
            assert!(allows(&v, 51413));
            assert!(!allows(&v, 22));
            assert!(!allows(&serde_json::json!({}), 51413));
        }

        #[test]
        fn changes_are_the_system_managers() {
            let v: serde_json::Value = serde_json::from_str(&changes(51413, true)).unwrap();
            assert_eq!(v[0], serde_json::json!({"change": "firewall-port", "port": 51413, "protocol": "tcp", "open": true}));
            assert_eq!(v[1]["protocol"], "udp");
        }
    }
}
