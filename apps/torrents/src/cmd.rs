//! Running external commands without blocking the UI thread.

pub use nyx_core::cmd::*;

use anyhow::Result;
use gtk::{gio, glib};

/// Run a command off the main thread and deliver the result back on it.
pub fn run_async<F>(args: &[&str], done: F)
where
    F: FnOnce(Result<String>) + 'static,
{
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    background(
        move || {
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            run(&refs)
        },
        done,
    );
}

/// Run arbitrary blocking work off the main thread, then call `done` on it.
pub fn background<T, W, F>(work: W, done: F)
where
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    F: FnOnce(T) + 'static,
{
    let handle = gio::spawn_blocking(work);
    glib::spawn_future_local(async move {
        if let Ok(value) = handle.await {
            done(value);
        }
    });
}
