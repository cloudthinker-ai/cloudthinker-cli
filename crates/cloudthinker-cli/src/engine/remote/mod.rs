pub mod bridge;
pub mod catalog;
pub mod client;
pub mod clipboard;
pub mod host;
pub mod keys;
pub mod paths;
pub mod picker;
pub mod targets;
pub mod tty;
pub mod wire;

use std::time::{SystemTime, UNIX_EPOCH};

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}
