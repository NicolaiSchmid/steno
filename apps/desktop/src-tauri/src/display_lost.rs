//! Linux: the display connection closing under the app. GDK ends the
//! process with `_exit(1)` as soon as it loses its X server or Wayland
//! compositor (GTK 3.24's `gdk_x_io_error` in `gdk/x11/gdkmain-x11.c`; the
//! event source's failure paths in `gdk/wayland/gdkeventsource.c`), which
//! is what a logout does on every desktop once the session's windows are
//! gone, and what a system shutdown does once logind goes ahead. Each of
//! those paths first logs one line through `GLib`, in the `Gdk` domain, and
//! `GLib` hands it to the process's log writer on the thread that hit the
//! loss, before the `_exit`. The shell's writer (`watch`) runs the
//! shutdown there, on that thread's behalf, and only then hands the line
//! to `GLib`'s default writer and lets GDK end the process: so a recording
//! in progress is saved first, at most `SHUTDOWN_PATIENCE` later, however
//! the session ended.
//!
//! The save needs nothing of the main thread, which is blocked meanwhile:
//! `App::shutdown` runs on a thread of its own (`ExitGate`), and the host
//! queues what it publishes for the main thread without waiting. On X11
//! both the main thread and tao's device thread (a second X connection)
//! hit the loss, usually within a millisecond; each one waits for the same
//! shutdown, so neither ends the process before it is over. A shutdown
//! already running (a SIGTERM's, a logout's) is waited for, not run again.
//!
//! GTK 3 is built with structured logging (`G_LOG_USE_STRUCTURED`), so the
//! lines reach only the writer, never a handler `g_log_set_handler`
//! installs; a process has one writer, and nothing else in the shell, tao,
//! wry or `WebKitGTK` sets one. Every other line goes to `GLib`'s default
//! writer unchanged.
//!
//! Swift: none; `AppKit` keeps the window server for the whole logout.

use std::sync::OnceLock;

use glib::{LogField, LogLevel, LogWriterOutput};

/// The app the writer saves, set once the app is built (`watch`); a loss
/// before that has no recording to save.
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Installs the writer: once per process, from `main`, before Tauri
/// starts GTK. `app` arrives later through `arm`.
pub fn watch() {
    glib::log_set_writer_func(write);
}

/// Hands the writer the app to save.
pub fn arm(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
}

/// The writer: saves first when `fields` are GDK's last line, then writes
/// them as `GLib` would. It must not unwind into `GLib`, which would abort the
/// process before the save, so a panic in the save is caught and the line
/// still written.
fn write(level: LogLevel, fields: &[LogField<'_>]) -> LogWriterOutput {
    let field = |key: &str| {
        fields
            .iter()
            .find(|field| field.key() == key)
            .and_then(LogField::value_str)
    };
    if let Some(app) = APP.get()
        && display_lost(field("GLIB_DOMAIN"), field("MESSAGE").unwrap_or_default())
    {
        stderr_line!("[steno-desktop] the display closed; saving before the app ends");
        let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::host::host(app).quit_pipeline();
            crate::shut_down_before_exit(app);
        }));
        if saved.is_err() {
            stderr_line!("[steno-desktop] the display closed and the save before the exit failed");
        }
    }
    glib::log_writer_default(level, fields)
}

/// Whether a line in `domain` saying `message` is the one GDK logs right
/// before it ends the process for a lost display. The fixed parts only:
/// the X11 line starts with the program's name and both carry
/// `g_strerror`'s text, which is translated. GTK 3.24.52:
///
/// - X11 (`gdk_x_io_error`): `<prgname>: Fatal IO error <n> (<error>) on
///   X server <display>.`, at debug level.
/// - Wayland (`gdk_event_source_prepare`, `_check`, `_dispatch` and
///   `_gdk_wayland_display_queue_events`): `Error flushing display:
///   <error>`, `Error reading events from display: <error>`, `Error <n>
///   (<error>) dispatching to Wayland display.` and `Lost connection to
///   Wayland compositor.`
fn display_lost(domain: Option<&str>, message: &str) -> bool {
    domain == Some("Gdk")
        && ((message.contains(": Fatal IO error ") && message.contains(" on X server "))
            || message.starts_with("Error flushing display:")
            || message.starts_with("Error reading events from display:")
            || (message.starts_with("Error ")
                && message.contains(" dispatching to Wayland display"))
            || message.starts_with("Lost connection to Wayland compositor"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GDK's last lines, as GTK 3.24.52 wrote them under Xvfb, weston and
    /// sway when the server went away.
    #[test]
    fn gdks_lines_for_a_lost_display_are_recognised() {
        for message in [
            "steno-desktop: Fatal IO error 11 (Resource temporarily unavailable) on X server :93.\n",
            "steno-desktop: Fatal IO error 32 (Datenübergabe unterbrochen) on X server :0.",
            "Error flushing display: Broken pipe",
            "Error reading events from display: Broken pipe",
            "Error 32 (Broken pipe) dispatching to Wayland display.",
            "Error 71 (Protocol error) dispatching to Wayland display.",
            "Lost connection to Wayland compositor.",
        ] {
            assert!(display_lost(Some("Gdk"), message), "{message}");
        }
    }

    /// Another domain, or another line of GDK's, is written as it is.
    #[test]
    fn other_lines_are_not_a_lost_display() {
        assert!(!display_lost(
            Some("GLib"),
            "Lost connection to Wayland compositor."
        ));
        assert!(!display_lost(
            None,
            "Error reading events from display: Broken pipe"
        ));
        for message in [
            "",
            "gdk_window_set_user_time called on non-toplevel",
            "Error fetching clipboard: no owner",
            "Fatal IO error on the wire",
        ] {
            assert!(!display_lost(Some("Gdk"), message), "{message}");
        }
    }
}
