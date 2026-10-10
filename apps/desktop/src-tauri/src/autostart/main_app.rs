//! Launch at login on macOS: `SMAppService.mainApp`, the Mac's own login
//! item and the one the Swift app registers (D4 of
//! `.plans/2026-10-07-stable-promotion.md`), through the safe
//! `smappservice-rs`. It is filed under the bundle id, so this app,
//! `com.nicolaischmid.steno.desktop`, registers itself; the Swift app's
//! entry is the system's to keep or drop, and nothing here touches it.
//!
//! At each launch, before the host's first-launch registration, the Launch
//! Agent a build under the earlier identifier wrote through
//! `tauri-plugin-autostart` (`~/Library/LaunchAgents/Steno.plist`,
//! [`EARLIER_AGENT_LABEL`]) goes, whatever the preferences say: unloaded
//! unless it is the job this process runs as, then deleted
//! ([`remove_earlier_agent`]). The registration is the host's
//! (`Host::register_login_item_on_first_launch` in `steno-host`): once, at
//! the first launch with the stored `launch_at_login` setting on, under the
//! preferences flag `steno.mainAppRegistered`; a failed registration tries
//! again at the next launch, and after that only the switch in General
//! registers or removes the login item ([`set_enabled`]).
//!
//! The rules run over two traits, [`MainApp`] and [`LaunchAgents`], which
//! the tests fake; the system's are [`SystemMainApp`] and
//! [`UserLaunchAgents`], macOS only. A smoke run (`STENO_SMOKE_SECONDS`)
//! registers and removes nothing, and only a run from an app bundle
//! outside a `fixture-host` build removes the agent
//! ([`removes_earlier_agent`]).
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

/// Logs what [`remove_earlier_agent`] did: each change and failure as a
/// warning, which the default filter shows; nothing when there was no
/// agent.
pub fn log(earlier_agent: &EarlierAgent) {
    match earlier_agent {
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
pub fn smoke_run() -> bool {
    std::env::var_os(crate::smoke::SECONDS_VARIABLE).is_some()
}

/// Whether the launch removes the earlier agent: not in a smoke run
/// (`smoke`), nor in a `fixture-host` build, nor when the executable `exe`
/// is not inside an app bundle ([`inside_app_bundle`]). A `cargo run`
/// binary under `target/` shares the home folder with an installed Steno
/// and must not remove the agent that Steno still starts at login.
pub fn removes_earlier_agent(smoke: bool, exe: Option<&std::path::Path>) -> bool {
    !smoke && !cfg!(feature = "fixture-host") && exe.is_some_and(inside_app_bundle)
}

/// Whether `exe` runs from an app bundle, `<name>.app/Contents/MacOS/`.
pub fn inside_app_bundle(exe: &std::path::Path) -> bool {
    let named = |dir: Option<&std::path::Path>, name: &str| {
        dir.and_then(std::path::Path::file_name)
            .is_some_and(|dir_name| dir_name == name)
    };
    let macos = exe.parent();
    let contents = macos.and_then(std::path::Path::parent);
    named(macos, "MacOS")
        && named(contents, "Contents")
        && contents
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::extension)
            .is_some_and(|extension| extension == "app")
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
    fn service_target(label: &str) -> String {
        // SAFETY: `getuid` takes no arguments, touches no memory and
        // cannot fail (POSIX).
        let uid = unsafe { libc::getuid() };
        format!("gui/{uid}/{label}")
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
            match Command::new("/bin/launchctl")
                .args(["print", &service_target(label)])
                .output()
            {
                Ok(output) if output.status.success() => Job::Loaded {
                    pid: pid_in(&String::from_utf8_lossy(&output.stdout)),
                },
                _ => Job::NotLoaded,
            }
        }

        fn bootout(&self, label: &str) -> Result<(), String> {
            let output = Command::new("/bin/launchctl")
                .args(["bootout", &service_target(label)])
                .output()
                .map_err(|error| error.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The system's agents folder and launchd, under a label no job
        /// has and in a folder of the test's own: the file is read and
        /// deleted, and launchd answers that no such job is loaded. Nothing
        /// is booted out or registered.
        #[test]
        fn the_system_agents_read_and_remove_by_label() {
            let folder = tempfile::tempdir().unwrap();
            let agents = UserLaunchAgents {
                directory: Some(folder.path().to_path_buf()),
            };
            let label = format!(
                "com.nicolaischmid.steno.desktop.test-{}",
                std::process::id()
            );
            assert_eq!(agents.read(&label).unwrap(), None);
            std::fs::write(folder.path().join(format!("{label}.plist")), "<plist/>").unwrap();
            assert_eq!(agents.read(&label).unwrap().as_deref(), Some("<plist/>"));
            assert_eq!(agents.job(&label), Job::NotLoaded);
            agents.remove(&label).unwrap();
            assert_eq!(agents.read(&label).unwrap(), None);
            let id = Command::new("/usr/bin/id").arg("-u").output().unwrap();
            let uid = String::from_utf8(id.stdout).unwrap();
            assert_eq!(
                service_target(&label),
                format!("gui/{}/{label}", uid.trim())
            );
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
    }

    impl FakeMainApp {
        fn new(status: LoginItemStatus, registers_as: Result<LoginItemStatus, String>) -> Self {
            FakeMainApp {
                status: Cell::new(status),
                registers_as,
            }
        }
    }

    impl MainApp for FakeMainApp {
        fn status(&self) -> LoginItemStatus {
            self.status.get()
        }

        fn register(&self) -> Result<(), String> {
            let status = self.registers_as.clone()?;
            self.status.set(status);
            Ok(())
        }

        fn unregister(&self) -> Result<(), String> {
            self.status.set(LoginItemStatus::NotRegistered);
            Ok(())
        }
    }

    /// A build under the earlier identifier with launch at login on left
    /// its agent loaded (the app was opened from the Finder, or quit): the
    /// job is booted out and its file deleted.
    #[test]
    fn a_leftover_agent_is_booted_out_and_deleted() {
        let agents = FakeAgents::with(EARLIER_AGENT, Job::Loaded { pid: None });
        assert_eq!(
            remove_earlier_agent(&agents, OWN_PID),
            EarlierAgent::Removed(Unloaded::BootedOut)
        );
        assert_eq!(agents.calls(), ["read", "job", "bootout", "remove"]);
        assert!(agents.file.borrow().is_none());
    }

    /// The first login after the update: launchd started this process from
    /// the leftover agent. Booting it out would end the app, so only the
    /// file goes, and the next login starts the app once, as the login
    /// item.
    #[test]
    fn the_agent_this_process_runs_as_loses_its_file_only() {
        let agents = FakeAgents::with(EARLIER_AGENT, Job::Loaded { pid: Some(OWN_PID) });
        assert_eq!(
            remove_earlier_agent(&agents, OWN_PID),
            EarlierAgent::Removed(Unloaded::RunsThisProcess)
        );
        assert_eq!(agents.calls(), ["read", "job", "remove"]);
        assert!(agents.file.borrow().is_none());
    }

    /// An agent that is not loaded loses its file; no agent at all is
    /// only read for.
    #[test]
    fn an_unloaded_agent_loses_its_file_and_none_is_nothing() {
        let agents = FakeAgents::with(EARLIER_AGENT, Job::NotLoaded);
        assert_eq!(
            remove_earlier_agent(&agents, OWN_PID),
            EarlierAgent::Removed(Unloaded::NotLoaded)
        );
        assert!(agents.file.borrow().is_none());
        let none = FakeAgents::default();
        assert_eq!(remove_earlier_agent(&none, OWN_PID), EarlierAgent::None);
        assert_eq!(none.calls(), ["read"]);
    }

    /// A file under the label that starts another program is not Steno's
    /// and stays; one that cannot be read is reported.
    #[test]
    fn an_agent_that_is_not_stenos_stays() {
        let other = EARLIER_AGENT.replace("/Contents/MacOS/steno-desktop", "/Contents/MacOS/Steno");
        let agents = FakeAgents::with(&other, Job::Loaded { pid: None });
        assert_eq!(
            remove_earlier_agent(&agents, OWN_PID),
            EarlierAgent::NotSteno
        );
        assert_eq!(agents.calls(), ["read"]);
        assert!(agents.file.borrow().is_some());

        let unreadable = FakeAgents {
            unreadable: true,
            ..FakeAgents::default()
        };
        assert!(matches!(
            remove_earlier_agent(&unreadable, OWN_PID),
            EarlierAgent::Failed(_)
        ));
    }

    /// The leftover agent goes in `setup` before the host's launch, whose
    /// first-launch registration (`Host::register_login_item_on_first_launch`)
    /// would otherwise register the main app beside a Launch Agent that
    /// still starts the same binary.
    #[test]
    fn the_leftover_agent_goes_before_the_hosts_launch() {
        // Without the carriage returns a Windows checkout may add.
        let main = include_str!("../main.rs").replace("\r\n", "\n");
        let setup = &main[main.find("\nfn setup(").unwrap()..];
        let removal = setup
            .find("#[cfg(target_os = \"macos\")]\n    autostart::remove_earlier_agent();")
            .expect("setup removes the leftover agent on macOS");
        let launch = setup
            .find("host::host(handle).launch(runtime);")
            .expect("setup launches the host");
        assert!(removal < launch);
    }

    /// Only an executable inside an app bundle removes the earlier agent,
    /// and never in a smoke run or a `fixture-host` build: a `cargo run`
    /// binary does not, whatever it is named.
    #[test]
    fn only_a_run_from_an_app_bundle_removes_the_agent() {
        use std::path::Path;
        let bundled = Path::new("/Applications/Steno.app/Contents/MacOS/steno-desktop");
        assert!(inside_app_bundle(bundled));
        assert_eq!(
            removes_earlier_agent(false, Some(bundled)),
            !cfg!(feature = "fixture-host")
        );
        assert!(!removes_earlier_agent(true, Some(bundled)));
        assert!(!removes_earlier_agent(false, None));
        for exe in [
            "/Users/me/steno/target/debug/steno-desktop",
            "/Users/me/target/MacOS/steno-desktop",
            "/Users/me/Steno/Contents/MacOS/steno-desktop",
            "/Users/me/Steno.app/MacOS/steno-desktop",
            "/Users/me/Steno.app/Contents/steno-desktop",
            "steno-desktop",
        ] {
            assert!(!inside_app_bundle(Path::new(exe)), "{exe}");
            assert!(!removes_earlier_agent(false, Some(Path::new(exe))), "{exe}");
        }
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
        let main_app =
            FakeMainApp::new(LoginItemStatus::NotRegistered, Ok(LoginItemStatus::Enabled));
        set_enabled(&main_app, true).unwrap();
        assert_eq!(main_app.status(), LoginItemStatus::Enabled);
        set_enabled(&main_app, false).unwrap();
        assert_eq!(main_app.status(), LoginItemStatus::NotRegistered);

        let refused = FakeMainApp::new(
            LoginItemStatus::NotRegistered,
            Err("Operation not permitted".to_owned()),
        );
        assert_eq!(
            set_enabled(&refused, true),
            Err("Operation not permitted".to_owned())
        );
        assert_eq!(refused.status(), LoginItemStatus::NotRegistered);
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
