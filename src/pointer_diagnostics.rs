//! Bounded diagnostics for pointer WebSocket recovery.
//!
//! CouchOS discards plugin stderr and reserves stdout for its protocol, so
//! diagnostics are written to a private file under the device's tmpfs.

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const LOG_PATH: &str = "/tmp/couch-webos-pointer.log";
const MAX_LOG_BYTES: u64 = 64 * 1024;

// Main-thread and pointer-worker diagnostics share this append-only log.
static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Start a fresh log for this plugin process.
pub(crate) fn reset() {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(LOG_PATH)
    {
        let _ = writeln!(file, "{} diagnostics started", timestamp());
    }
}

/// Append one diagnostic line, truncating the file before it grows without bound.
pub(crate) fn record(message: &str) {
    let Ok(_guard) = LOG_LOCK.lock() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(LOG_PATH)
    else {
        return;
    };

    if file
        .metadata()
        .map(|metadata| metadata.len() > MAX_LOG_BYTES)
        .unwrap_or(false)
    {
        if file.set_len(0).is_err() {
            return;
        }
    }
    let _ = writeln!(file, "{} {}", timestamp(), message);
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| format!("{}.{:03}", duration.as_secs(), duration.subsec_millis()))
        .unwrap_or_else(|_| "time-unavailable".to_owned())
}
