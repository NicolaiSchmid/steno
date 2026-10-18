//! The tray icon: the Swift menu bar item's menu as a native menu on every
//! platform. On macOS it is the menu bar extra (a template icon, so it
//! follows the bar's appearance); on Linux it is a status notifier item and
//! on Windows a notification area icon, both with the app icon. The menu
//! is the Swift popover's: Record (or Stop), Record in person, the
//! meetings whose speakers wait for a review, the processing queue and the
//! five recent meetings (each opens its meeting), Open Steno, Settings,
//! Launch at login, Check for Updates, Quit (Exit on Windows). Pending
//! reviews also count on the icon: a number beside it on macOS and where a
//! Linux host shows a label, and in the tooltip everywhere. Only the Mac's
//! menu shows shortcut hints (`shortcut`).
//!
//! Every action is in the menu (`MENU`), and a click on the icon does
//! nothing the menu does not: macOS and Windows open the menu on a left
//! click as on a right one, and on Linux the host decides what a left
//! click does. KDE Plasma and GNOME's `AppIndicator` extension open the
//! menu; a host that sends `Activate` instead, as Omarchy's bar may, gets
//! an error back (libayatana-appindicator does not implement it), and
//! there only a right click opens the menu.
//!
//! The tray also keeps the process alive: with it, closing the main window
//! hides it and the process stays, as the Swift menu bar app stays; without
//! a tray the window closes and the process quits with it, saving a
//! recording in progress first (`main.rs`). On Linux a tray that was built
//! still counts as none while nothing shows its icon (`has_host`).
//!
//! The items are `actions::MenuAction`s and their handler is
//! `actions::on_menu_event`, registered once by `main.rs`: Tauri hands
//! every menu event to the same listeners, whether from the tray's menu
//! or, on macOS, the menu bar's (`menu.rs`), whose items carry the same
//! ids.
//!
//! Swift: `MenuBarView.swift`, `MenuBarLabel` and `MenuBarLabelPresentation`
//! in `StenoApp.swift` and `FloatingContent.swift`.

use std::sync::Mutex;

use tauri::{
    AppHandle, Manager, Wry,
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

use steno_bridge::Platform;
use steno_core::json::uuid_string;
use steno_host::tray::{TrayMeetings, TrayRow};

use crate::{
    actions::{MEETING_PREFIX, MenuAction},
    autostart,
    recording::{RecorderState, RecordingState},
};

/// The tray's id, for `AppHandle::tray_by_id`.
pub const TRAY_ID: &str = "steno";

/// The Record item's text per recorder state: the same words as the
/// sidebar's control (`RecordButton` in `sidebar.tsx`).
pub const fn record_label(state: RecordingState) -> &'static str {
    match state {
        RecordingState::Idle => "Record",
        RecordingState::Starting => "Starting…",
        RecordingState::Recording => "Stop recording",
        RecordingState::Stopping => "Stopping…",
    }
}

/// Whether the Record item does anything: not while the recorder is
/// between states.
pub const fn record_enabled(state: RecordingState) -> bool {
    matches!(state, RecordingState::Idle | RecordingState::Recording)
}

/// "Record in person" is offered only to an idle recorder
/// (`RecordingControlPresentation.offersInPerson`).
pub const fn in_person_enabled(state: RecordingState) -> bool {
    matches!(state, RecordingState::Idle)
}

/// What both Record rows read once Quit was chosen (`note_quitting`).
pub const QUITTING: &str = "Quitting…";

/// The two Record rows: Record (or Stop) and "Record in person", each
/// with its text and whether it is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordRows {
    pub record: (&'static str, bool),
    pub in_person: (&'static str, bool),
}

/// The Record rows for the recorder's `state`: once quitting, both read
/// "Quitting…", disabled, whatever the recorder does next, since a start
/// is refused from then on (`Recorder::refuse_starts`; Rust only: the
/// Swift app's Quit ended its menu).
pub const fn record_rows(state: RecordingState, quitting: bool) -> RecordRows {
    if quitting {
        RecordRows {
            record: (QUITTING, false),
            in_person: (QUITTING, false),
        }
    } else {
        RecordRows {
            record: (record_label(state), record_enabled(state)),
            in_person: (MenuAction::RecordInPerson.label(), in_person_enabled(state)),
        }
    }
}

/// The icon's tooltip, the Swift label's accessibility text, "Steno" or
/// "Steno, recording" (the elapsed time is the bubble's, not the tray's),
/// then the pending reviews: "Steno, 2 meetings to review".
pub fn tooltip(state: RecordingState, reviews: usize) -> String {
    let base = if state.is_busy() {
        "Steno, recording"
    } else {
        "Steno"
    };
    match reviews {
        0 => base.to_owned(),
        1 => format!("{base}, 1 meeting to review"),
        n => format!("{base}, {n} meetings to review"),
    }
}

/// The number beside the icon: the pending reviews, none at 0.
fn badge(reviews: usize) -> Option<String> {
    (reviews > 0).then(|| reviews.to_string())
}

/// One line of the meeting block, which sits between the Record rows and
/// Open Steno.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Separator,
    Header(&'static str),
    Meeting(TrayRow),
}

/// The meeting block: the reviews, then "Processing" over the queue, then
/// "Recent" over the recent meetings, each part that has rows after a
/// separator.
fn lines(meetings: &TrayMeetings) -> Vec<Line> {
    let mut lines = Vec::new();
    for (header, rows) in [
        (None, &meetings.reviews),
        (Some("Processing"), &meetings.queue),
        (Some("Recent"), &meetings.recent),
    ] {
        if rows.is_empty() {
            continue;
        }
        lines.push(Line::Separator);
        lines.extend(header.map(Line::Header));
        lines.extend(rows.iter().cloned().map(Line::Meeting));
    }
    lines
}

/// Whether two blocks have the same items and differ at most in a
/// meeting row's text, so the texts can be set in place; a progress event
/// moves only the percentage.
fn same_items(shown: &[Line], next: &[Line]) -> bool {
    shown.len() == next.len()
        && shown.iter().zip(next).all(|pair| match pair {
            (Line::Meeting(shown), Line::Meeting(next)) => shown.meeting_id == next.meeting_id,
            (shown, next) => shown == next,
        })
}

/// Where the meeting block starts in the menu: after the Record rows.
const BLOCK_AT: usize = 2;

/// A menu item of the meeting block.
enum BlockItem {
    Separator(PredefinedMenuItem<Wry>),
    Text(MenuItem<Wry>),
}

impl BlockItem {
    fn new(app: &AppHandle, line: &Line) -> tauri::Result<Self> {
        Ok(match line {
            Line::Separator => BlockItem::Separator(PredefinedMenuItem::separator(app)?),
            Line::Header(text) => {
                BlockItem::Text(MenuItem::with_id(app, *text, *text, false, None::<&str>)?)
            }
            Line::Meeting(row) => BlockItem::Text(MenuItem::with_id(
                app,
                format!("{MEETING_PREFIX}{}", uuid_string(row.meeting_id)),
                &row.text,
                true,
                None::<&str>,
            )?),
        })
    }

    fn item(&self) -> &dyn IsMenuItem<Wry> {
        match self {
            BlockItem::Separator(item) => item,
            BlockItem::Text(item) => item,
        }
    }
}

/// The meeting block as shown, and what the icon's tooltip says.
struct Block {
    lines: Vec<Line>,
    items: Vec<BlockItem>,
    recording: RecordingState,
    reviews: usize,
}

impl Default for Block {
    fn default() -> Self {
        Block {
            lines: Vec::new(),
            items: Vec::new(),
            recording: RecordingState::Idle,
            reviews: 0,
        }
    }
}

/// The rows that change while the app runs: the two Record rows, and
/// whether Launch at login can be clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rows {
    record: RecordRows,
    launch_at_login: bool,
}

/// What the changing rows show, apart from the menu items so a test can
/// drive it. The Record rows follow the recorder until Quit, then read
/// "Quitting…" ([`record_rows`]); Launch at login, switchable or not from
/// the build on, is disabled at Quit too, as a switch clicked then would
/// land beside the exit's own (`autostart::at_exit`) or not at all. It
/// remembers the rows last set, so a `recording` snapshot that moved only
/// the levels (20 a second) sets nothing.
#[derive(Debug)]
struct RowState {
    /// Whether the system lets Launch at login be switched
    /// (`autostart::switchable`).
    switchable: bool,
    /// Set once, by [`Self::quit`].
    quitting: bool,
    shown: Option<Rows>,
}

impl RowState {
    fn new(switchable: bool) -> Self {
        RowState {
            switchable,
            quitting: false,
            shown: None,
        }
    }

    /// A `recording` snapshot in `state`: the rows to set, none when they
    /// already read so.
    fn recording(&mut self, state: RecordingState) -> Option<Rows> {
        self.show(state)
    }

    /// Quit was chosen: the rows to set, none when they already read so.
    fn quit(&mut self) -> Option<Rows> {
        self.quitting = true;
        self.show(RecordingState::Idle)
    }

    fn show(&mut self, state: RecordingState) -> Option<Rows> {
        let rows = Rows {
            record: record_rows(state, self.quitting),
            launch_at_login: self.switchable && !self.quitting,
        };
        (self.shown.replace(rows) != Some(rows)).then_some(rows)
    }
}

/// The live items, kept as managed state so a `recording` snapshot can
/// retitle them. The Record rows are the tray's and, on macOS, the menu
/// bar's Record menu's (`menu::RecordMenu`).
pub struct Tray {
    menu: Menu<Wry>,
    record: Vec<MenuItem<Wry>>,
    in_person: Vec<MenuItem<Wry>>,
    launch_at_login: CheckMenuItem<Wry>,
    rows: Mutex<RowState>,
    block: Mutex<Block>,
}

/// A menu item's shortcut hint, on the Mac only: the menu bar extra's
/// menu shows one as the Swift menus did, while Windows' notification
/// area menu and a Linux status notifier menu show none, and a key
/// pressed there with the menu closed does nothing.
const fn shortcut(platform: Platform, action: MenuAction) -> Option<&'static str> {
    match (platform, action) {
        (Platform::Macos, MenuAction::Record) => Some("Cmd+Shift+R"),
        (Platform::Macos, MenuAction::OpenSettings) => Some("Cmd+,"),
        (Platform::Macos, MenuAction::Quit) => Some("Cmd+Q"),
        _ => None,
    }
}

/// One row of the tray's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Item(MenuAction),
    Separator,
}

/// The tray's menu, top to bottom, without the meeting block: every
/// `MenuAction` but Find Meetings once, so nothing waits on a click the
/// host may not pass on. Quit is the shell's own
/// item, not muda's predefined Quit, which ends the process without
/// `ExitRequested`, so the shell could not shut down cleanly.
const MENU: [Row; 10] = [
    Row::Item(MenuAction::Record),
    Row::Item(MenuAction::RecordInPerson),
    Row::Separator,
    Row::Item(MenuAction::OpenMain),
    Row::Item(MenuAction::OpenSettings),
    Row::Separator,
    Row::Item(MenuAction::LaunchAtLogin),
    Row::Item(MenuAction::CheckForUpdates),
    Row::Separator,
    Row::Item(MenuAction::Quit),
];

/// Builds the menu and the icon and manages `Tray`.
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let platform = Platform::CURRENT;
    let record = MenuAction::Record.item(app, shortcut(platform, MenuAction::Record))?;
    let in_person = MenuAction::RecordInPerson.item(app, None)?;
    // A login item the system manages shows checked and cannot be
    // switched (`autostart`).
    let login_item = autostart::status(app);
    let switchable = autostart::switchable(login_item);
    let launch_at_login = CheckMenuItem::with_id(
        app,
        MenuAction::LaunchAtLogin.as_str(),
        MenuAction::LaunchAtLogin.label(),
        switchable,
        login_item.is_on(),
        None::<&str>,
    )?;
    let menu = Menu::new(app)?;
    for row in MENU {
        match row {
            Row::Separator => menu.append(&PredefinedMenuItem::separator(app)?)?,
            Row::Item(MenuAction::Record) => menu.append(&record)?,
            Row::Item(MenuAction::RecordInPerson) => menu.append(&in_person)?,
            Row::Item(MenuAction::LaunchAtLogin) => menu.append(&launch_at_login)?,
            Row::Item(action) => menu.append(&action.item(app, shortcut(platform, action))?)?,
        }
    }
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .tooltip(tooltip(RecordingState::Idle, 0));
    builder = if cfg!(target_os = "macos") {
        // The four-bar mark as a template image: the menu bar draws it in
        // its own colour, as it drew the SF Symbol.
        builder
            .icon(Image::from_bytes(include_bytes!(
                "../icons/tray/mark@2x.png"
            ))?)
            .icon_as_template(true)
    } else if let Some(icon) = app.default_window_icon() {
        builder.icon(icon.clone())
    } else {
        builder
    };
    builder.build(app)?;
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let (mut records, mut in_persons) = (vec![record], vec![in_person]);
    #[cfg(target_os = "macos")]
    if let Some(bar) = app.try_state::<crate::menu::RecordMenu>() {
        records.push(bar.record.clone());
        in_persons.push(bar.in_person.clone());
    }
    app.manage(Tray {
        menu,
        record: records,
        in_person: in_persons,
        launch_at_login,
        rows: Mutex::new(RowState::new(switchable)),
        block: Mutex::default(),
    });
    Ok(())
}

/// The host's meeting rows changed (`Host::on_tray`), on the main thread:
/// the meeting block and the badge follow.
pub fn note_meetings(app: &AppHandle, meetings: &TrayMeetings) {
    let Some(tray) = app.try_state::<Tray>() else {
        return;
    };
    let mut block = lock(&tray.block);
    let next = lines(meetings);
    if same_items(&block.lines, &next) {
        for ((item, shown), line) in block.items.iter().zip(&block.lines).zip(&next) {
            if let (BlockItem::Text(item), Line::Meeting(row)) = (item, line)
                && shown != line
            {
                let _ = item.set_text(&row.text);
            }
        }
    } else {
        for item in block.items.drain(..) {
            let _ = tray.menu.remove(item.item());
        }
        for line in &next {
            match BlockItem::new(app, line) {
                Ok(item) => {
                    let _ = tray.menu.insert(item.item(), BLOCK_AT + block.items.len());
                    block.items.push(item);
                }
                Err(error) => tracing::warn!(%error, "a tray row could not be built"),
            }
        }
    }
    block.lines = next;
    let reviews = meetings.reviews.len();
    if block.reviews != reviews {
        block.reviews = reviews;
        if let Some(icon) = app.tray_by_id(TRAY_ID) {
            let _ = icon.set_title(badge(reviews));
            let _ = icon.set_tooltip(Some(tooltip(block.recording, reviews)));
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A `recording` snapshot reached the main window: retitle the items.
pub fn note_recording(app: &AppHandle, state: RecordingState) {
    let Some(tray) = app.try_state::<Tray>() else {
        return;
    };
    let change = tray.row_state().recording(state);
    set_rows(&tray, change);
    let mut block = lock(&tray.block);
    if block.recording != state {
        block.recording = state;
        if let Some(icon) = app.tray_by_id(TRAY_ID) {
            let _ = icon.set_tooltip(Some(tooltip(state, block.reviews)));
        }
    }
}

/// Quit was chosen: both Record rows read "Quitting…", disabled, from now
/// on ([`record_rows`]), so a Record that would be refused is never
/// clicked unseen, and Launch at login is disabled ([`RowState`]). On the
/// main thread, before the exit.
pub fn note_quitting(app: &AppHandle) {
    if let Some(tray) = app.try_state::<Tray>() {
        let change = tray.row_state().quit();
        set_rows(&tray, change);
    }
}

impl Tray {
    fn row_state(&self) -> std::sync::MutexGuard<'_, RowState> {
        lock(&self.rows)
    }
}

/// Sets the rows a change asked for; nothing for none. On the main thread,
/// as every change is, so they land in the order they were made.
fn set_rows(tray: &Tray, change: Option<Rows>) {
    let Some(Rows {
        record,
        launch_at_login,
    }) = change
    else {
        return;
    };
    for (items, (text, enabled)) in [
        (&tray.record, record.record),
        (&tray.in_person, record.in_person),
    ] {
        for item in items {
            let _ = item.set_text(text);
            let _ = item.set_enabled(enabled);
        }
    }
    let _ = tray.launch_at_login.set_enabled(launch_at_login);
}

/// The login item changed (from the menu or from Settings): the check mark
/// follows what the system says, `on`, read off the main thread by the
/// job that switched it, not what was asked.
pub fn note_login_item(app: &AppHandle, on: bool) {
    if let Some(tray) = app.try_state::<Tray>() {
        let _ = tray.launch_at_login.set_checked(on);
    }
}

/// Whether something on the desktop shows the tray's icon. macOS and
/// Windows always do. On Linux a status notifier host does, as the
/// follower in `tray_host` last read it; until it has, and with no session
/// bus, none does, the safe side: closing main then ends the app instead
/// of leaving it running unseen. Reading it asks nothing of the bus.
pub fn has_host() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::tray_host::shown()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_mac_menu_shows_shortcuts() {
        assert_eq!(shortcut(Platform::Macos, MenuAction::Quit), Some("Cmd+Q"));
        assert_eq!(
            shortcut(Platform::Macos, MenuAction::Record),
            Some("Cmd+Shift+R")
        );
        assert_eq!(
            shortcut(Platform::Macos, MenuAction::OpenSettings),
            Some("Cmd+,")
        );
        assert_eq!(shortcut(Platform::Macos, MenuAction::OpenMain), None);
        for &action in MenuAction::ALL {
            assert_eq!(shortcut(Platform::Windows, action), None);
            assert_eq!(shortcut(Platform::Linux, action), None);
        }
    }

    /// Every action but Find Meetings is in the tray's menu once, so a
    /// host that passes no left click on still reaches each; separators
    /// only between items.
    #[test]
    fn the_menu_holds_every_action_once() {
        for &action in MenuAction::ALL {
            let rows = MENU.iter().filter(|&&row| row == Row::Item(action));
            let expected = usize::from(action != MenuAction::FindMeetings);
            assert_eq!(rows.count(), expected, "{action}");
        }
        assert_eq!(
            MENU[..BLOCK_AT],
            [
                Row::Item(MenuAction::Record),
                Row::Item(MenuAction::RecordInPerson)
            ],
            "the meeting block follows the Record rows"
        );
        assert_ne!(MENU.first(), Some(&Row::Separator));
        assert_ne!(MENU.last(), Some(&Row::Separator));
        assert!(
            MENU.windows(2)
                .all(|pair| pair != [Row::Separator, Row::Separator])
        );
    }

    #[test]
    fn the_record_item_follows_the_recorder() {
        assert_eq!(record_label(RecordingState::Idle), "Record");
        assert_eq!(record_label(RecordingState::Starting), "Starting…");
        assert_eq!(record_label(RecordingState::Recording), "Stop recording");
        assert_eq!(record_label(RecordingState::Stopping), "Stopping…");
        assert!(record_enabled(RecordingState::Idle));
        assert!(record_enabled(RecordingState::Recording));
        assert!(!record_enabled(RecordingState::Starting));
        assert!(!record_enabled(RecordingState::Stopping));
    }

    /// Once quitting, the Record rows read "Quitting…", disabled, for
    /// every state the recorder moves through on the way out.
    #[test]
    fn the_record_rows_read_quitting_once_quit_was_chosen() {
        for state in [
            RecordingState::Idle,
            RecordingState::Starting,
            RecordingState::Recording,
            RecordingState::Stopping,
        ] {
            let quitting = record_rows(state, true);
            assert_eq!(quitting.record, ("Quitting…", false), "{state:?}");
            assert_eq!(quitting.in_person, ("Quitting…", false), "{state:?}");
            let rows = record_rows(state, false);
            assert_eq!(rows.record, (record_label(state), record_enabled(state)));
            assert_eq!(
                rows.in_person,
                ("Record in person", in_person_enabled(state))
            );
        }
    }

    /// A `recording` snapshot that moves nothing the rows show (the
    /// levels, 20 a second) sets no row, and every one that does sets
    /// them, back to the first rows set as well.
    #[test]
    fn only_a_snapshot_that_moves_a_row_sets_the_rows() {
        use RecordingState::{Idle, Recording, Stopping};
        let mut rows = RowState::new(true);
        for state in [Idle, Recording, Stopping, Idle, Recording] {
            assert_eq!(
                rows.recording(state),
                Some(Rows {
                    record: record_rows(state, false),
                    launch_at_login: true,
                }),
                "{state:?}"
            );
            assert_eq!(rows.recording(state), None, "{state:?} again");
        }
    }

    /// Quit sets the quitting rows (`note_quitting`): both Record rows
    /// read "Quitting…", disabled, and Launch at login is disabled; every
    /// snapshot after it (`note_recording`) leaves them so, whatever the
    /// recorder does on the way out. A login item the system manages
    /// stays disabled throughout.
    #[test]
    fn quit_sets_the_quitting_rows_and_the_snapshots_after_it_keep_them() {
        use RecordingState::{Idle, Recording, Starting, Stopping};
        for switchable in [true, false] {
            let mut rows = RowState::new(switchable);
            assert_eq!(
                rows.recording(Recording).map(|rows| rows.launch_at_login),
                Some(switchable)
            );
            let quitting = Rows {
                record: record_rows(Idle, true),
                launch_at_login: false,
            };
            assert_eq!(quitting.record.record, ("Quitting…", false));
            assert_eq!(rows.quit(), Some(quitting));
            for state in [Stopping, Idle, Starting, Recording] {
                assert_eq!(rows.recording(state), None, "{state:?}");
            }
            assert_eq!(rows.quit(), None);
            assert_eq!(rows.shown, Some(quitting));
        }
    }

    #[test]
    fn in_person_is_offered_to_an_idle_recorder_only() {
        assert!(in_person_enabled(RecordingState::Idle));
        for state in [
            RecordingState::Starting,
            RecordingState::Recording,
            RecordingState::Stopping,
        ] {
            assert!(!in_person_enabled(state), "{state:?}");
        }
    }

    #[test]
    fn the_tooltip_is_the_swift_accessibility_label_and_the_reviews() {
        assert_eq!(tooltip(RecordingState::Idle, 0), "Steno");
        assert_eq!(tooltip(RecordingState::Starting, 0), "Steno, recording");
        assert_eq!(tooltip(RecordingState::Recording, 0), "Steno, recording");
        assert_eq!(
            tooltip(RecordingState::Idle, 1),
            "Steno, 1 meeting to review"
        );
        assert_eq!(
            tooltip(RecordingState::Recording, 2),
            "Steno, recording, 2 meetings to review"
        );
        assert_eq!(badge(0), None);
        assert_eq!(badge(3).as_deref(), Some("3"));
    }

    fn row(n: u128, text: &str) -> TrayRow {
        TrayRow {
            meeting_id: uuid::Uuid::from_u128(n),
            text: text.to_owned(),
        }
    }

    /// Each part with rows comes after a separator, the queue and the
    /// recent meetings under their header; a block with no rows is empty.
    #[test]
    fn the_meeting_block_lists_each_part_that_has_rows() {
        assert_eq!(lines(&TrayMeetings::default()), []);
        let meetings = TrayMeetings {
            queue: vec![row(1, "Standup — Transcribing… 10%")],
            recent: vec![row(2, "Weekly sync")],
            reviews: vec![row(2, "Review speakers in Weekly sync")],
        };
        assert_eq!(
            lines(&meetings),
            [
                Line::Separator,
                Line::Meeting(meetings.reviews[0].clone()),
                Line::Separator,
                Line::Header("Processing"),
                Line::Meeting(meetings.queue[0].clone()),
                Line::Separator,
                Line::Header("Recent"),
                Line::Meeting(meetings.recent[0].clone()),
            ]
        );
    }

    /// A progress event moves a row's text only, which is set in place; a
    /// meeting that joins or leaves rebuilds the block.
    #[test]
    fn only_a_new_meeting_rebuilds_the_block() {
        let queued = |text| TrayMeetings {
            queue: vec![row(1, text)],
            ..TrayMeetings::default()
        };
        let shown = lines(&queued("Standup — Transcribing… 10%"));
        assert!(same_items(
            &shown,
            &lines(&queued("Standup — Transcribing… 40%"))
        ));
        let mut two = queued("Standup — Transcribing… 40%");
        two.queue.push(row(3, "Retro — Waiting to process"));
        assert!(!same_items(&shown, &lines(&two)));
        assert!(!same_items(&shown, &lines(&TrayMeetings::default())));
    }

    #[test]
    fn the_template_icon_is_a_png() {
        let bytes = include_bytes!("../icons/tray/mark@2x.png");
        assert_eq!(&bytes[1..4], b"PNG");
        assert!(Image::from_bytes(bytes).is_ok());
    }
}
