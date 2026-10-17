//! Minimal local diagnostics: no torrent paths, tracker URLs or session settings.
use std::{
    fs::OpenOptions,
    io::Write,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

static LOG_LOCK: Mutex<()> = Mutex::new(());

pub fn event(message: &str) {
    let _guard = LOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let path = exe.with_file_name("ratio-mini-diagnostic.log");
    // Keep this file bounded, including across repeated launches.
    let truncate = std::fs::metadata(&path).is_ok_and(|m| m.len() > 65_536);
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .open(path)
    else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = writeln!(file, "{timestamp} {} {message}", env!("CARGO_PKG_VERSION"));
}

pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The panic payload could contain private network data; record only its location.
        if let Some(location) = info.location() {
            event(&format!(
                "Rust panic at {}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            ));
        } else {
            event("Rust panic (location unavailable)");
        }
        previous(info);
    }));
}
