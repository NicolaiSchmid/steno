//! Safe wrappers over the Mac framework calls that have no safe binding:
//! `calendar` (`EventKit` access and events), `keychain` (PKCS#12 export and
//! the keychain test's fixture calls) and `volume` (free space for important
//! use). Every item is macOS only; each `unsafe` block sits in a safe
//! function with a `SAFETY:` comment for every invariant it relies on.

#![deny(unsafe_code)]

#[cfg(target_os = "macos")]
pub mod calendar;
#[cfg(target_os = "macos")]
pub mod keychain;
#[cfg(target_os = "macos")]
pub mod volume;
