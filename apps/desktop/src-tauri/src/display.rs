//! The GDK backend on Linux. On Wayland GTK 3 cannot place a top-level
//! window, keep it above others or report moves, so the panels would not
//! float; under `XWayland` all three work. With `XWayland` available the
//! shell therefore allows GDK only `x11`, inside this process. A user's own
//! `GDK_BACKEND` wins unless it is a session default (`is_session_default`).

use std::ffi::OsStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// No Wayland session: GTK picks, which is X11.
    GtksChoice,
    /// A Wayland session with `XWayland`: forced to `x11`.
    ForcedX11,
    /// A Wayland session without `XWayland`: the panels neither stay on
    /// top nor keep their place.
    WaylandOnly,
    /// The user's own `GDK_BACKEND`.
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

    /// The backends the shell allows GDK, or `None` to leave the choice to
    /// GTK or the user.
    pub const fn allowed_backends(self) -> Option<&'static str> {
        match self {
            Self::ForcedX11 => Some("x11"),
            Self::GtksChoice | Self::WaylandOnly | Self::UserChoice => None,
        }
    }
}

/// The GDK backend for these `WAYLAND_DISPLAY`, `DISPLAY` and
/// `GDK_BACKEND` values; an empty value counts as none, except for the
/// user's `GDK_BACKEND`.
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
/// choice: `*`, or a list with an entry `x11` or `*` (`wayland,x11,*`).
/// A list without either stays the user's: narrowed to `x11` it would
/// leave GDK no backend to open.
fn is_session_default(gdk_backend: &OsStr) -> bool {
    let value = gdk_backend.as_encoded_bytes();
    let list = value.contains(&b',');
    value
        .split(|&byte| byte == b',')
        .any(|entry| entry == b"*" || (list && entry == b"x11"))
}

/// Applies `backend` to this process and logs it, before Tauri initialises
/// GTK.
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

    /// A Wayland session runs under `XWayland` when available, unless the
    /// user chose a backend.
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
            // A list naming `x11` or `*` is a session default.
            (wayland, x11, Some("wayland,x11,*"), ForcedX11),
            (wayland, x11, Some("wayland,x11"), ForcedX11),
            (wayland, x11, Some("*"), ForcedX11),
            (wayland, x11, Some("wayland,"), UserChoice),
            (wayland, x11, Some("wayland,broadway"), UserChoice),
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

    /// Which `GDK_BACKEND` values count as a session default.
    #[test]
    fn a_list_naming_x11_or_any_backend_is_a_session_default() {
        for value in [
            "wayland,x11,*",
            "x11,wayland",
            "wayland,x11",
            "*",
            "wayland,*",
        ] {
            assert!(is_session_default(OsStr::new(value)), "{value}");
        }
        for value in [
            "wayland",
            "x11",
            "",
            "broadway",
            "wayland,",
            "wayland,broadway",
            "wayland, x11",
            "wayland,x11x",
            "wayland,X11",
        ] {
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
