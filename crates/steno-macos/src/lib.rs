//! Safe wrappers over the Mac framework calls that have no safe binding in
//! `security-framework` or `objc2`, or a binding that is safe but for a
//! constant. Plan: `.plans/2026-10-07-stable-promotion.md` (D10).
//!
//! | Module | What it wraps |
//! |--------|---------------|
//! | `calendar` | `EventKit`: the calendar permission's status and prompt, and the timed events around a recording's start, read as plain values (the meeting's title and attendees, S3) |
//! | `keychain` | `SecItemExport` of an identity as PKCS#12 with a passphrase (the import of the Swift app's handover identity, S6), and behind the `testing` feature the keychain test's fixture calls: `SecItemImport` of a SEC1 key into a given keychain, `SecItemUpdate` of an item's label, `SecKeychainDelete` |
//! | `volume` | A volume's free space for important use and its size, read on an `NSURL` (the recorder's disk floor, P20) |
//!
//! Every item is macOS only, so the module names are plain text: on Linux
//! and Windows the crate is empty and its docs list nothing. Each `unsafe`
//! block sits in a safe function and carries a `SAFETY:` comment for every
//! invariant it relies on; nothing else in the workspace calls these
//! frameworks raw.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
pub mod calendar;
#[cfg(target_os = "macos")]
pub mod keychain;
#[cfg(target_os = "macos")]
pub mod volume;
