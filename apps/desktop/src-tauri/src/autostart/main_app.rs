//! Launch at login on macOS: `SMAppService.mainApp`, the Mac's own login
//! item and the one the Swift app registers (D4 of
//! `.plans/2026-10-07-stable-promotion.md`), through the safe
//! `smappservice-rs`. It is filed under the bundle id, so this app,
//! `com.nicolaischmid.steno.desktop`, registers itself; the Swift app's
//! entry is the system's to keep or drop, and nothing here touches it.
//!
//! At each launch ([`at_launch`]), first the Launch Agent a build under
//! the earlier identifier wrote through `tauri-plugin-autostart`
//! (`~/Library/LaunchAgents/Steno.plist`, [`EARLIER_AGENT_LABEL`]) goes,
//! whatever the preferences say: unloaded unless it is the job this
//! process runs as, then deleted ([`remove_earlier_agent`]). Then, while
//! the stored `launch_at_login` setting is on and the login item is
//! neither enabled nor awaiting the user's approval, the app registers
//! itself ([`register_when_on`]). The setting is the user's word, kept
//! in the shared database by the switch in General, so the preferences'
//! `steno.loginItemRegistered`, which a build under the earlier
//! identifier set for its Launch Agent, decides nothing here. A login
//! item the user turned off in System Settings reads `requiresApproval`
//! and stays off.
//!
//! The decisions run over two traits, [`MainApp`] and [`LaunchAgents`],
//! which the tests fake; the system's are [`SystemMainApp`] and
//! [`UserLaunchAgents`], macOS only. A smoke run (`STENO_SMOKE_SECONDS`)
//! registers and removes nothing.
//!
//! Swift: `LoginItemController.swift`.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use steno_host::services::LoginItemStatus;

/// The label `tauri-plugin-autostart` gave its Launch Agent, and the
/// agent's file name before `.plist`: the product name (the plugin's
/// default, `PackageInfo::name`), "Steno" on macOS. A build under the
/// earlier identifier wrote it.
pub const EARLIER_AGENT_LABEL: &str = "Steno";

/// What the plugin's agent starts: this app's executable inside a bundle.
/// A file under the label that starts anything else is not Steno's.
const AGENT_PROGRAM_SUFFIX: &str = "/Contents/MacOS/steno-desktop</string>";

/// `SMAppService.mainApp`.
pub trait MainApp {
    fn status(&self) -> LoginItemStatus;
    fn register(&self) -> Result<(), String>;
    fn unregister(&self) -> Result<(), String>;
}

/// A job launchd holds in this user's GUI domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    NotLoaded,
    /// Loaded, with the process it runs when one runs.
    Loaded {
        pid: Option<u32>,
    },
}

/// `~/Library/LaunchAgents` and launchd, by label.
pub trait LaunchAgents {
    /// The text of the agent's file; none when there is no file.
    fn read(&self, label: &str) -> std::io::Result<Option<String>>;
    fn remove(&self, label: &str) -> std::io::Result<()>;
    fn job(&self, label: &str) -> Job;
    /// `launchctl bootout`, which also ends the job's process.
    fn bootout(&self, label: &str) -> Result<(), String>;
}

/// What became of the earlier build's Launch Agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlierAgent {
    None,
    /// A file under the label that starts another program: left alone.
    NotSteno,
    /// The file is gone; what became of the loaded job.
    Removed(Unloaded),
    /// The file could not be read or deleted.
    Failed(String),
}

/// What became of the earlier agent's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unloaded {
    NotLoaded,
    BootedOut,
    /// The job is this process, which launchd started at login from that
    /// file: booting it out would end the app. It stays loaded until the
    /// user logs out, and with its file gone nothing starts it again.
    RunsThisProcess,
    Failed(String),
}

/// Removes the earlier build's Launch Agent: its job unloaded, unless
/// it is `own_pid`, then its file deleted.
pub fn remove_earlier_agent(agents: &dyn LaunchAgents, own_pid: u32) -> EarlierAgent {
    let label = EARLIER_AGENT_LABEL;
    let text = match agents.read(label) {
        Ok(None) => return EarlierAgent::None,
        Ok(Some(text)) => text,
        Err(error) => return EarlierAgent::Failed(error.to_string()),
    };
    if !text.contains(AGENT_PROGRAM_SUFFIX) {
        return EarlierAgent::NotSteno;
    }
    let unloaded = match agents.job(label) {
        Job::NotLoaded => Unloaded::NotLoaded,
        Job::Loaded { pid: Some(pid) } if pid == own_pid => Unloaded::RunsThisProcess,
        Job::Loaded { .. } => match agents.bootout(label) {
            Ok(()) => Unloaded::BootedOut,
            Err(error) => Unloaded::Failed(error),
        },
    };
    match agents.remove(label) {
        Ok(()) => EarlierAgent::Removed(unloaded),
        Err(error) => EarlierAgent::Failed(error.to_string()),
    }
}

/// What the launch did about the login item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Registration {
    /// Launch at login is off, or the setting could not be read.
    Off,
    /// Already enabled, or awaiting the user's approval.
    Kept(LoginItemStatus),
    /// Registered; the status after it (`RequiresApproval` until the user
    /// allows it in System Settings).
    Registered(LoginItemStatus),
    Failed(String),
}

/// Registers the login item while `launch_at_login` is on and it is
/// neither enabled nor awaiting approval.
pub fn register_when_on(main_app: &dyn MainApp, launch_at_login: bool) -> Registration {
    if !launch_at_login {
        return Registration::Off;
    }
    match main_app.status() {
        status @ (LoginItemStatus::Enabled | LoginItemStatus::RequiresApproval) => {
            Registration::Kept(status)
        }
        _ => match main_app.register() {
            Ok(()) => Registration::Registered(main_app.status()),
            Err(error) => Registration::Failed(error),
        },
    }
}

/// What [`at_launch`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub earlier_agent: EarlierAgent,
    pub registration: Registration,
}

/// The launch's login item step: the earlier agent removed first, then
/// the registration while `launch_at_login` (the stored setting, none
/// when it could not be read) is on.
pub fn at_launch(
    agents: &dyn LaunchAgents,
    main_app: &dyn MainApp,
    launch_at_login: Option<bool>,
    own_pid: u32,
) -> Launch {
    let earlier_agent = remove_earlier_agent(agents, own_pid);
    let registration = register_when_on(main_app, launch_at_login.unwrap_or(false));
    Launch {
        earlier_agent,
        registration,
    }
}

/// Logs what [`at_launch`] did: each change and failure as a warning,
/// which the default filter shows; nothing when nothing changed.
pub fn log(launch: &Launch) {
    match &launch.earlier_agent {
        EarlierAgent::None => {}
        EarlierAgent::NotSteno => tracing::warn!(
            "~/Library/LaunchAgents/{EARLIER_AGENT_LABEL}.plist starts another program; left alone"
        ),
        EarlierAgent::Removed(unloaded) => tracing::warn!(
            ?unloaded,
            "removed the Launch Agent an earlier Steno left behind"
        ),
        EarlierAgent::Failed(error) => tracing::warn!(
            %error,
            "the Launch Agent an earlier Steno left behind could not be removed"
        ),
    }
    match &launch.registration {
        Registration::Off | Registration::Kept(_) => {}
        Registration::Registered(status) => {
            tracing::warn!(?status, "registered Steno as a login item");
        }
        Registration::Failed(error) => {
            tracing::warn!(%error, "Steno could not register as a login item");
        }
    }
}

/// The switch in General and the tray: register or unregister, as the
/// Swift app's `setEnabled` does.
pub fn set_enabled(main_app: &dyn MainApp, enabled: bool) -> Result<(), String> {
    if enabled {
        main_app.register()
    } else {
        main_app.unregister()
    }
}

/// The pid in `launchctl print`'s description of a job: its `pid = <n>`
/// line, there while the job's process runs.
pub fn pid_in(description: &str) -> Option<u32> {
    description
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = "))
        .and_then(|pid| pid.trim().parse().ok())
}

/// Whether this is a smoke run, which registers and removes nothing.
fn smoke_run() -> bool {
    std::env::var_os(crate::smoke::SECONDS_VARIABLE).is_some()
}

#[cfg(target_os = "macos")]
pub use system::{SystemMainApp, UserLaunchAgents};

#[cfg(target_os = "macos")]
mod system {
    use std::path::PathBuf;
    use std::process::Command;

    use smappservice_rs::{AppService, ServiceStatus, ServiceType};
    use steno_host::services::LoginItemStatus;

    use super::{Job, LaunchAgents, MainApp, pid_in, smoke_run};

    /// The system's `SMAppService.mainApp`. A smoke run, whose binary is
    /// no installed app, never registers or unregisters it.
    pub struct SystemMainApp;

    impl MainApp for SystemMainApp {
        fn status(&self) -> LoginItemStatus {
            match AppService::new(ServiceType::MainApp).status() {
                ServiceStatus::NotRegistered => LoginItemStatus::NotRegistered,
                ServiceStatus::Enabled => LoginItemStatus::Enabled,
                ServiceStatus::RequiresApproval => LoginItemStatus::RequiresApproval,
                ServiceStatus::NotFound => LoginItemStatus::NotFound,
            }
        }

        fn register(&self) -> Result<(), String> {
            if smoke_run() {
                return Err("A smoke run registers no login item.".to_owned());
            }
            AppService::new(ServiceType::MainApp)
                .register()
                .map_err(|error| error.to_string())
        }

        fn unregister(&self) -> Result<(), String> {
            if smoke_run() {
                return Err("A smoke run removes no login item.".to_owned());
            }
            AppService::new(ServiceType::MainApp)
                .unregister()
                .map_err(|error| error.to_string())
        }
    }

    /// `$HOME/Library/LaunchAgents`, where the plugin wrote (it took the
    /// home from `HOME`), and launchd's GUI domain of this user.
    pub struct UserLaunchAgents {
        directory: Option<PathBuf>,
    }

    impl UserLaunchAgents {
        pub fn of_home() -> Self {
            let directory = std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join("Library").join("LaunchAgents"));
            UserLaunchAgents { directory }
        }

        fn file(&self, label: &str) -> Option<PathBuf> {
            Some(self.directory.as_ref()?.join(format!("{label}.plist")))
        }
    }

    /// `gui/<uid>/<label>`, launchd's name for the job in this user's GUI
    /// domain.
    fn service_target(label: &str) -> Result<String, String> {
        let output = Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .map_err(|error| error.to_string())?;
        let uid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || uid.parse::<u32>().is_err() {
            return Err(format!("id -u answered {uid:?}"));
        }
        Ok(format!("gui/{uid}/{label}"))
    }

    impl LaunchAgents for UserLaunchAgents {
        fn read(&self, label: &str) -> std::io::Result<Option<String>> {
            let Some(file) = self.file(label) else {
                return Ok(None);
            };
            match std::fs::read(file) {
                Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }

        fn remove(&self, label: &str) -> std::io::Result<()> {
            match self.file(label) {
                Some(file) => std::fs::remove_file(file),
                None => Ok(()),
            }
        }

        fn job(&self, label: &str) -> Job {
            let Ok(target) = service_target(label) else {
                return Job::NotLoaded;
            };
            match Command::new("/bin/launchctl")
                .args(["print", &target])
                .output()
            {
                Ok(output) if output.status.success() => Job::Loaded {
                    pid: pid_in(&String::from_utf8_lossy(&output.stdout)),
                },
                _ => Job::NotLoaded,
            }
        }

        fn bootout(&self, label: &str) -> Result<(), String> {
            let target = service_target(label)?;
            let output = Command::new("/bin/launchctl")
                .args(["bootout", &target])
                .output()
                .map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// The agent the plugin wrote for a build under the earlier
    /// identifier (`auto-launch`'s template, `MacosLauncher::LaunchAgent`).
    const EARLIER_AGENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
  <key>Label</key>
  <string>Steno</string>
  <key>AssociatedBundleIdentifiers</key>
  <array></array>
  <key>ProgramArguments</key>
  <array><string>/Applications/Steno.app/Contents/MacOS/steno-desktop</string></array>
  <key>RunAtLoad</key>
  <true/>
  </dict>
</plist>"#;

    const OWN_PID: u32 = 4242;

    /// A Launch Agents folder and launchd: one file and one job by label,
    /// every call recorded.
    #[derive(Default)]
    struct FakeAgents {
        file: RefCell<Option<String>>,
        job: Cell<Option<Job>>,
        unreadable: bool,
        bootout_fails: bool,
        calls: RefCell<Vec<String>>,
    }

    impl FakeAgents {
        fn with(text: &str, job: Job) -> Self {
            FakeAgents {
                file: RefCell::new(Some(text.to_owned())),
                job: Cell::new(Some(job)),
                ..FakeAgents::default()
            }
        }

        fn note(&self, call: &str, label: &str) {
            assert_eq!(label, "Steno");
            self.calls.borrow_mut().push(call.to_owned());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl LaunchAgents for FakeAgents {
        fn read(&self, label: &str) -> std::io::Result<Option<String>> {
            self.note("read", label);
            if self.unreadable {
                return Err(std::io::Error::other("permission denied"));
            }
            Ok(self.file.borrow().clone())
        }

        fn remove(&self, label: &str) -> std::io::Result<()> {
            self.note("remove", label);
            self.file.borrow_mut().take();
            Ok(())
        }

        fn job(&self, label: &str) -> Job {
            self.note("job", label);
            self.job.get().unwrap_or(Job::NotLoaded)
        }

        fn bootout(&self, label: &str) -> Result<(), String> {
            self.note("bootout", label);
            if self.bootout_fails {
                return Err("Boot-out failed: 5: Input/output error".to_owned());
            }
            self.job.set(Some(Job::NotLoaded));
            Ok(())
        }
    }

    /// `SMAppService.mainApp`: a status, and what `register` turns it into
    /// or the error it fails with.
    struct FakeMainApp {
        status: Cell<LoginItemStatus>,
        registers_as: Result<LoginItemStatus, String>,
        registered: Cell<u32>,
    }

    impl FakeMainApp {
        fn new(status: LoginItemStatus, registers_as: Result<LoginItemStatus, String>) -> Self {
            FakeMainApp {
                status: Cell::new(status),
                registers_as,
                registered: Cell::new(0),
            }
        }
    }

    impl MainApp for FakeMainApp {
        fn status(&self) -> LoginItemStatus {
            self.status.get()
        }

        fn register(&self) -> Result<(), String> {
            self.registered.set(self.registered.get() + 1);
            let status = self.registers_as.clone()?;
            self.status.set(status);
            Ok(())
        }

        fn unregister(&self) -> Result<(), String> {
            self.status.set(LoginItemStatus::NotRegistered);
            Ok(())
        }
    }

    fn not_registered() -> FakeMainApp {
        FakeMainApp::new(LoginItemStatus::NotRegistered, Ok(LoginItemStatus::Enabled))
    }

    /// A build under the earlier identifier with launch at login on left
    /// its agent loaded (the app was opened from the Finder, or quit): the
    /// job is booted out, its file deleted, and then the app registers.
    #[test]
    fn a_leftover_agent_is_removed_then_the_app_registers() {
        let agents = FakeAgents::with(EARLIER_AGENT, Job::Loaded { pid: None });
        let main_app = not_registered();
        let launch = at_launch(&agents, &main_app, Some(true), OWN_PID);
        assert_eq!(
            launch,
            Launch {
                earlier_agent: EarlierAgent::Removed(Unloaded::BootedOut),
                registration: Registration::Registered(LoginItemStatus::Enabled),
            }
        );
        assert_eq!(agents.calls(), ["read", "job", "bootout", "remove"]);
        assert!(agents.file.borrow().is_none());
        assert_eq!(main_app.registered.get(), 1);
    }

    /// The first login after the update: launchd started this process from
    /// the leftover agent. Booting it out would end the app, so only the
    /// file goes, and the next login starts the app once, as the login
    /// item.
    #[test]
    fn the_agent_this_process_runs_as_loses_its_file_only() {
        let agents = FakeAgents::with(EARLIER_AGENT, Job::Loaded { pid: Some(OWN_PID) });
        let launch = at_launch(&agents, &not_registered(), Some(true), OWN_PID);
        assert_eq!(
            launch.earlier_agent,
            EarlierAgent::Removed(Unloaded::RunsThisProcess)
        );
        assert_eq!(agents.calls(), ["read", "job", "remove"]);
        assert!(agents.file.borrow().is_none());
    }

    /// Launch at login off: the leftover agent goes all the same, and
    /// nothing registers; with the setting unreadable, nothing registers
    /// either.
    #[test]
    fn off_removes_the_leftover_agent_and_registers_nothing() {
        for setting in [Some(false), None] {
            let agents = FakeAgents::with(EARLIER_AGENT, Job::NotLoaded);
            let main_app = not_registered();
            let launch = at_launch(&agents, &main_app, setting, OWN_PID);
            assert_eq!(
                launch,
                Launch {
                    earlier_agent: EarlierAgent::Removed(Unloaded::NotLoaded),
                    registration: Registration::Off,
                }
            );
            assert!(agents.file.borrow().is_none());
            assert_eq!(main_app.registered.get(), 0);
            assert_eq!(main_app.status(), LoginItemStatus::NotRegistered);
        }
    }

    /// Nothing to remove, an item already enabled, and one the user has
    /// yet to allow (or turned off) in System Settings: nothing registers
    /// again, and the status the General section shows is the system's.
    #[test]
    fn an_enabled_or_awaiting_login_item_is_kept() {
        for status in [LoginItemStatus::Enabled, LoginItemStatus::RequiresApproval] {
            let agents = FakeAgents::default();
            let main_app = FakeMainApp::new(status, Err("not called".to_owned()));
            let launch = at_launch(&agents, &main_app, Some(true), OWN_PID);
            assert_eq!(launch.earlier_agent, EarlierAgent::None);
            assert_eq!(launch.registration, Registration::Kept(status));
            assert_eq!(main_app.registered.get(), 0);
            assert_eq!(agents.calls(), ["read"]);
        }
    }

    /// A registration the system leaves awaiting approval says so: the
    /// General section shows "requires approval" with the button to System
    /// Settings. A status the system cannot find registers too.
    #[test]
    fn a_registration_awaiting_approval_is_shown_as_such() {
        let main_app = FakeMainApp::new(
            LoginItemStatus::NotFound,
            Ok(LoginItemStatus::RequiresApproval),
        );
        assert_eq!(
            register_when_on(&main_app, true),
            Registration::Registered(LoginItemStatus::RequiresApproval)
        );
        assert!(main_app.status().is_on());
    }

    /// A refused registration is reported with the system's reason, and
    /// the status stays what it was, so General shows the switch off.
    #[test]
    fn a_failed_registration_is_surfaced() {
        let main_app = FakeMainApp::new(
            LoginItemStatus::NotRegistered,
            Err("Operation not permitted".to_owned()),
        );
        let launch = at_launch(&FakeAgents::default(), &main_app, Some(true), OWN_PID);
        assert_eq!(
            launch.registration,
            Registration::Failed("Operation not permitted".to_owned())
        );
        assert_eq!(main_app.status(), LoginItemStatus::NotRegistered);
        assert!(set_enabled(&main_app, true).is_err());
    }

    /// A file under the label that starts another program is not Steno's
    /// and stays; one that cannot be read is reported. Neither stops the
    /// registration.
    #[test]
    fn an_agent_that_is_not_stenos_stays() {
        let other = EARLIER_AGENT.replace("/Contents/MacOS/steno-desktop", "/Contents/MacOS/Steno");
        let agents = FakeAgents::with(&other, Job::Loaded { pid: None });
        let main_app = not_registered();
        let launch = at_launch(&agents, &main_app, Some(true), OWN_PID);
        assert_eq!(launch.earlier_agent, EarlierAgent::NotSteno);
        assert_eq!(agents.calls(), ["read"]);
        assert!(agents.file.borrow().is_some());
        assert_eq!(
            launch.registration,
            Registration::Registered(LoginItemStatus::Enabled)
        );

        let unreadable = FakeAgents {
            unreadable: true,
            ..FakeAgents::default()
        };
        assert!(matches!(
            remove_earlier_agent(&unreadable, OWN_PID),
            EarlierAgent::Failed(_)
        ));
    }

    /// A boot-out that fails still deletes the file, so the next login
    /// does not load the agent.
    #[test]
    fn a_failed_bootout_still_deletes_the_file() {
        let agents = FakeAgents {
            bootout_fails: true,
            ..FakeAgents::with(EARLIER_AGENT, Job::Loaded { pid: Some(7) })
        };
        assert_eq!(
            remove_earlier_agent(&agents, OWN_PID),
            EarlierAgent::Removed(Unloaded::Failed(
                "Boot-out failed: 5: Input/output error".to_owned()
            ))
        );
        assert!(agents.file.borrow().is_none());
    }

    #[test]
    fn the_switch_registers_and_unregisters() {
        let main_app = not_registered();
        set_enabled(&main_app, true).unwrap();
        assert_eq!(main_app.status(), LoginItemStatus::Enabled);
        set_enabled(&main_app, false).unwrap();
        assert_eq!(main_app.status(), LoginItemStatus::NotRegistered);
    }

    #[test]
    fn the_pid_is_read_from_launchctl_print() {
        let running = "gui/501/Steno = {\n\tactive count = 1\n\tpath = /Users/ada/Library/LaunchAgents/Steno.plist\n\tstate = running\n\n\tprogram = /Applications/Steno.app/Contents/MacOS/steno-desktop\n\tpid = 812\n\tlast exit code = (never exited)\n}\n";
        assert_eq!(pid_in(running), Some(812));
        let idle = "gui/501/Steno = {\n\tactive count = 0\n\tstate = not running\n\tlast exit code = 0\n}\n";
        assert_eq!(pid_in(idle), None);
    }

    /// The real `SMAppService.mainApp` status of the test binary, read
    /// without registering anything: an unbundled binary has no login
    /// item, so it is not enabled.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_status_reads_without_registering() {
        let status = SystemMainApp.status();
        assert_ne!(status, LoginItemStatus::Enabled, "{status:?}");
        assert_ne!(status, LoginItemStatus::Managed);
    }
}
