//! The GDK backend on Linux. On Wayland GTK 3 can neither place a
//! top-level window nor keep it above the others, and reports no moves, so
//! the panels would neither float nor stay where they are put nor save
//! their anchor; under `XWayland` all three work. On a Wayland session with
//! `XWayland` the shell therefore allows GDK only its `x11` backend, before
//! Tauri initialises GTK. That is a setting inside this process, so nothing
//! the shell starts (the browser behind `xdg-open`) inherits it. A
//! single `GDK_BACKEND` the user set always wins; a list (`wayland,x11,*`,
//! which Omarchy sets for the whole session) is a session default, which
//! the shell narrows to `x11` as if none were set.
//!
//! Swift: none; `AppKit` has a single window server.

use std::ffi::OsStr;

/// Which GDK backend the shell runs on (`backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// No Wayland session: GTK picks, which is X11.
    GtksChoice,
    /// A Wayland session with `XWayland`: the shell allows GDK only `x11`
    /// and runs under it.
    ForcedX11,
    /// A Wayland session without `XWayland` (no `DISPLAY`): X11 would not
    /// open, so GTK runs on Wayland and the panels neither stay on top nor
    /// keep their place.
    WaylandOnly,
    /// The user's `GDK_BACKEND`: one backend, or empty.
    UserChoice,
}

impl Backend {
    /// The startup log line; it names no value from the environment.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::GtksChoice => "display: GTK's default backend (no Wayland session)",
            Self::ForcedX11 => {
                "display: a Wayland session, so the shell runs under XWayland \
                 (GDK's x11 backend) to keep the panels on top and where they are put; \
                 a single GDK_BACKEND set before launch overrides this"
            }
            Self::WaylandOnly => {
                "display: a Wayland session without XWayland, so the panels \
                 neither stay on top nor keep their place"
            }
            Self::UserChoice => "display: the GDK_BACKEND set before launch",
        }
    }

    /// The backends the shell allows GDK (`gdk_set_allowed_backends`), or
    /// `None` to leave the choice to GTK or to the user's `GDK_BACKEND`,
    /// which the shell never narrows.
    pub const fn allowed_backends(self) -> Option<&'static str> {
        match self {
            Self::ForcedX11 => Some("x11"),
            Self::GtksChoice | Self::WaylandOnly | Self::UserChoice => None,
        }
    }
}

/// The GDK backend for a session with these `WAYLAND_DISPLAY`, `DISPLAY`
/// and `GDK_BACKEND` values (an empty value counts as none, except the
/// user's `GDK_BACKEND`, which wins even when it is `wayland` or empty;
/// a list counts as none, `is_session_default`).
pub fn backend(
    wayland_display: Option<&OsStr>,
    x11_display: Option<&OsStr>,
    gdk_backend: Option<&OsStr>,
) -> Backend {
    let set = |value: Option<&OsStr>| value.is_some_and(|value| !value.is_empty());
    if gdk_backend.is_some_and(|value| !is_session_default(value)) {
        Backend::UserChoice
    } else if !set(wayland_display) {
        Backend::GtksChoice
    } else if set(x11_display) {
        Backend::ForcedX11
    } else {
        Backend::WaylandOnly
    }
}

/// Whether a `GDK_BACKEND` is a session's default rather than the user's
/// choice: a list of backends (`wayland,x11,*`) or one with GDK's `*` (any
/// backend). A desktop that exports such a list for every app, as Omarchy
/// does, only states a preference; the panels need `x11`, and
/// `gdk_set_allowed_backends` keeps GDK to it, since GDK skips every entry
/// of the list it does not allow.
fn is_session_default(gdk_backend: &OsStr) -> bool {
    gdk_backend
        .as_encoded_bytes()
        .iter()
        .any(|byte| matches!(byte, b',' | b'*'))
}

/// Applies `backend` to this process and logs it once. Runs first in
/// `main`, before Tauri initialises GTK, which is when GDK reads the list.
pub fn choose() {
    let backend = backend(
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("GDK_BACKEND").as_deref(),
    );
    if let Some(allowed) = backend.allowed_backends() {
        gdk::set_allowed_backends(allowed);
    }
    stderr_line!("[steno-desktop] {}", backend.describe());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Wayland session runs under `XWayland` when there is one, unless
    /// the user chose a single backend; an X11 session, or an empty
    /// `WAYLAND_DISPLAY`, is left to GTK.
    #[test]
    fn a_wayland_session_runs_under_xwayland_unless_the_user_chose() {
        use Backend::{ForcedX11, GtksChoice, UserChoice, WaylandOnly};
        let wayland = Some("wayland-0");
        let x11 = Some(":0");
        for (wayland_display, x11_display, gdk_backend, expected) in [
            (wayland, None, None, WaylandOnly),
            (None, x11, None, GtksChoice),
            (wayland, x11, None, ForcedX11),
            (wayland, x11, Some("wayland"), UserChoice),
            (None, x11, Some("x11"), UserChoice),
            (wayland, x11, Some(""), UserChoice),
            (Some(""), x11, None, GtksChoice),
            (wayland, Some(""), None, WaylandOnly),
            // A list is the session's default (Omarchy's), not a choice.
            (wayland, x11, Some("wayland,x11,*"), ForcedX11),
            (wayland, x11, Some("wayland,x11"), ForcedX11),
            (wayland, x11, Some("*"), ForcedX11),
            (wayland, None, Some("wayland,x11,*"), WaylandOnly),
            (None, x11, Some("wayland,x11,*"), GtksChoice),
        ] {
            assert_eq!(
                backend(
                    wayland_display.map(OsStr::new),
                    x11_display.map(OsStr::new),
                    gdk_backend.map(OsStr::new),
                ),
                expected,
                "{wayland_display:?} {x11_display:?} {gdk_backend:?}"
            );
            assert!(expected.describe().starts_with("display: "));
        }
    }

    /// A list or GDK's `*` is a session default; one backend, or none
    /// named (empty), is the user's.
    #[test]
    fn a_list_or_any_backend_is_a_session_default() {
        for value in ["wayland,x11,*", "x11,wayland", "wayland,", "*", "wayland,*"] {
            assert!(is_session_default(OsStr::new(value)), "{value}");
        }
        for value in ["wayland", "x11", "", "broadway"] {
            assert!(!is_session_default(OsStr::new(value)), "{value}");
        }
    }

    /// Only the shell's own choice narrows GDK, and to `x11` alone.
    #[test]
    fn only_xwayland_narrows_the_backends_and_to_x11() {
        assert_eq!(Backend::ForcedX11.allowed_backends(), Some("x11"));
        for backend in [
            Backend::GtksChoice,
            Backend::WaylandOnly,
            Backend::UserChoice,
        ] {
            assert_eq!(backend.allowed_backends(), None, "{backend:?}");
        }
    }
}
