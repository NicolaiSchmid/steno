//! Linux: the display connection closing under the app, as at a logout or
//! shutdown. GDK then logs one line and calls `_exit(1)`. The shell's
//! `GLib` log writer (`write`) recognises that line and runs the shutdown
//! first, on the logging thread, so a recording in progress is saved. The
//! save needs nothing of the main thread, which is blocked meanwhile.

use std::sync::OnceLock;

use gio::glib::{self, LogField, LogLevel, LogWriterOutput};

/// The app the writer saves, set once the app is built; a loss before that
/// has no recording to save.
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

/// Installs the writer once per process, from `main`, before Tauri starts
/// GTK.
pub fn watch() {
    glib::log_set_writer_func(|level, fields| {
        write(
            level,
            fields,
            APP.get().map(|app| || crate::save_before_end(app)),
        )
    });
}

pub fn arm(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
}

/// The writer: runs `save` first on GDK's last line, then writes the line
/// as `GLib` would. A panic in the save is caught, since unwinding into
/// `GLib` would abort before the line is out.
fn write(level: LogLevel, fields: &[LogField<'_>], save: Option<impl FnOnce()>) -> LogWriterOutput {
    if let Some(save) = save
        && lost_display(fields)
    {
        stderr_line!("[steno-desktop] the display closed; saving before the app ends");
        let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(save));
        if saved.is_err() {
            stderr_line!("[steno-desktop] the display closed and the save before the exit failed");
        }
    }
    glib::log_writer_default(level, fields)
}

/// Whether `fields` are the line GDK logs before it ends the process for a
/// lost display: domain `Gdk` and one of GTK 3.24.52's X11 or Wayland
/// messages, matched on their untranslated fixed parts. A GTK that rewords
/// them ends the process unsaved again.
fn lost_display(fields: &[LogField<'_>]) -> bool {
    let field = |key: &str| {
        fields
            .iter()
            .find(|field| field.key() == key)
            .and_then(LogField::value_str)
    };
    let message = field("MESSAGE").unwrap_or_default();
    field("GLIB_DOMAIN") == Some("Gdk")
        && ((message.contains(": Fatal IO error ") && message.contains(" on X server "))
            || message.starts_with("Error flushing display:")
            || message.starts_with("Error reading events from display:")
            || (message.starts_with("Error ")
                && message.contains(" dispatching to Wayland display"))
            || message.starts_with("Lost connection to Wayland compositor"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gio::glib::gstr;

    use super::*;

    /// A line as `GLib` hands it to the writer.
    fn line<'a>(domain: Option<&'a str>, message: &'a str) -> Vec<LogField<'a>> {
        let mut fields = vec![
            LogField::new(gstr!("PRIORITY"), b"7"),
            LogField::new(gstr!("MESSAGE"), message.as_bytes()),
        ];
        if let Some(domain) = domain {
            fields.push(LogField::new(gstr!("GLIB_DOMAIN"), domain.as_bytes()));
        }
        fields
    }

    /// GDK's last lines as GTK 3.24.52 wrote them under Xvfb, weston and
    /// sway.
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
            assert!(lost_display(&line(Some("Gdk"), message)), "{message}");
        }
    }

    /// Any other line is written as it is.
    #[test]
    fn other_lines_are_not_a_lost_display() {
        assert!(!lost_display(&line(
            Some("GLib"),
            "Lost connection to Wayland compositor."
        )));
        assert!(!lost_display(&line(
            None,
            "Error reading events from display: Broken pipe"
        )));
        assert!(!lost_display(&[
            LogField::new(gstr!("DOMAIN"), b"Gdk"),
            LogField::new(gstr!("MESSAGE"), b"Lost connection to Wayland compositor."),
        ]));
        for message in [
            "",
            "gdk_window_set_user_time called on non-toplevel",
            "Error fetching clipboard: no owner",
            "Fatal IO error on the wire",
            "steno-desktop: Fatal IO error 11 (Resource temporarily unavailable).",
            "steno-desktop: lost the selection on X server :0.",
            "Warning 32 dispatching to Wayland display.",
            "Error 32 (Broken pipe) dispatching to the compositor.",
        ] {
            assert!(!lost_display(&line(Some("Gdk"), message)), "{message}");
        }
    }

    /// The writer saves on GDK's last line only, and a panicking save
    /// still writes the line.
    #[test]
    fn the_writer_saves_before_gdks_last_line_only() {
        let saves = Cell::new(0);
        let save = || saves.set(saves.get() + 1);
        let lost = line(Some("Gdk"), "Lost connection to Wayland compositor.");
        let other = line(Some("Gdk"), "Error fetching clipboard: no owner");
        write(LogLevel::Debug, &other, Some(save));
        assert_eq!(saves.get(), 0);
        write(LogLevel::Debug, &lost, None::<fn()>);
        assert_eq!(saves.get(), 0);
        let written = write(LogLevel::Debug, &lost, Some(save));
        assert_eq!(saves.get(), 1);
        assert_eq!(written, LogWriterOutput::Handled);
        let written = write(LogLevel::Debug, &lost, Some(|| panic!("the save failed")));
        assert_eq!(written, LogWriterOutput::Handled);
    }
}
