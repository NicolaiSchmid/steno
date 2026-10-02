//! Where the engine puts the detail it must not send: a failing write,
//! hash, promote, intake or store call carries the inbox path (and with it
//! the user's home directory) in its description. The phone and the receipt
//! get a fixed phrase; the detail goes here, on this computer only, through
//! `tracing` for whoever subscribes (the shell, the CLI).
//! Swift: `HandoverLog.swift`.

use std::fmt::Display;

pub(crate) fn failure(what: &str, error: &dyn Display) {
    tracing::error!(target: "steno::handover", "{what} failed: {error}");
}
