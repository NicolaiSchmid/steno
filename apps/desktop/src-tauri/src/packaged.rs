//! Launch at login on a packaged install (stable plan X5). The updates
//! half of X5 is `steno_services::updates::updates_are_managed`.
//!
//! - **Managed by the system** ([`login_item_is_managed`]):
//!   `STENO_LOGIN_ITEM=managed`, which the NixOS module sets for its user
//!   service and the session. The login item is then the system's: the app
//!   never writes, rewrites or removes its autostart entry (`autostart`
//!   reports `LoginItemStatus::Managed` and changes nothing), except that at
//!   launch it removes an entry an earlier build wrote whose `Exec` starts a
//!   program in the Nix store ([`remove_store_entry`]), which would start a
//!   second copy beside the service, without the wrapper's environment, and
//!   break once the store path is collected.
//! - **The entry's path** ([`launcher_path`], Linux): the autostart entry
//!   the app writes names a path that stays the same across upgrades,
//!   never `current_exe()` (on Nix the wrapped binary inside the store):
//!   [`EXEC_PATH_VARIABLE`] when a package's wrapper names one, else the
//!   first of [`candidates`] that resolves into the directory the running
//!   binary resolves into ([`stable_path`]). Nix's wrapper
//!   (`<out>/bin/steno-desktop`) and the binary it runs
//!   (`<out>/bin/.steno-desktop-wrapped`) share that directory, so a
//!   profile's `bin/steno-desktop` counts while it links to this build. An
//!   `AppImage` keeps the plugin's own entry, which names `$APPIMAGE`. With no
//!   such path the app writes no entry and logs why.
//!
//! Rust only: the Swift app is a bundle `SMAppService` registers.

use std::ffi::OsStr;

/// The variable a package sets to say the system starts the app at login
/// ([`login_item_is_managed`]).
pub const LOGIN_ITEM_VARIABLE: &str = "STENO_LOGIN_ITEM";

/// Whether the system starts the app at login and owns the setting:
/// [`LOGIN_ITEM_VARIABLE`] is `managed`.
pub fn login_item_is_managed() -> bool {
    managed(std::env::var_os(LOGIN_ITEM_VARIABLE).as_deref())
}

fn managed(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("managed"))
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    /// The variable a package's wrapper sets to the path the autostart
    /// entry names, for a launcher that is not beside the binary it runs
    /// (the AUR package's `/usr/bin` wrapper, stable plan X6).
    pub const EXEC_PATH_VARIABLE: &str = "STENO_EXEC_PATH";

    /// The binary's name, in every package.
    const BINARY: &str = "steno-desktop";

    /// Where Nix keeps its builds, which a garbage collection removes.
    const STORE: &str = "/nix/store/";

    /// Where a package puts the app's launcher, in the order they are
    /// tried: `/usr/bin` (the `.deb` and the AUR package), the NixOS
    /// system profile, the per-user profile NixOS and Home Manager fill
    /// (for `user`), and the user's Nix profile (under `home`).
    pub fn candidates(user: Option<&str>, home: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = vec![
            Path::new("/usr/bin").join(BINARY),
            Path::new("/run/current-system/sw/bin").join(BINARY),
        ];
        if let Some(user) = user.filter(|user| !user.is_empty() && !user.contains('/')) {
            paths.push(
                Path::new("/etc/profiles/per-user")
                    .join(user)
                    .join("bin")
                    .join(BINARY),
            );
        }
        if let Some(home) = home {
            paths.push(home.join(".nix-profile/bin").join(BINARY));
        }
        paths
    }

    /// The first of `candidates` that launches this build: one that
    /// exists and, with every link resolved, lies in the directory that
    /// `current_exe` resolves into. The candidate itself is returned, not
    /// where it resolves to.
    pub fn stable_path(candidates: &[PathBuf], current_exe: &Path) -> Option<PathBuf> {
        let resolved = current_exe.canonicalize().ok()?;
        let directory = resolved.parent()?;
        candidates
            .iter()
            .find(|candidate| {
                candidate
                    .canonicalize()
                    .is_ok_and(|target| target.parent() == Some(directory))
            })
            .cloned()
    }

    /// The path [`EXEC_PATH_VARIABLE`] names, when it is an absolute path
    /// to a file outside the Nix store.
    pub fn named_path(value: &OsStr) -> Option<PathBuf> {
        let path = PathBuf::from(value);
        (path.is_absolute() && !path.starts_with(STORE) && path.is_file()).then_some(path)
    }

    /// The path the autostart entry names (see the module doc), `None`
    /// when there is none.
    pub fn launcher_path() -> Option<PathBuf> {
        if let Some(value) = std::env::var_os(EXEC_PATH_VARIABLE) {
            if let Some(path) = named_path(&value) {
                return Some(path);
            }
            tracing::warn!(
                "{EXEC_PATH_VARIABLE} names no file outside the Nix store; looking for the launcher instead"
            );
        }
        let current_exe = std::env::current_exe().ok()?;
        let user = std::env::var("USER").ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        stable_path(&candidates(user.as_deref(), home.as_deref()), &current_exe)
    }

    /// The autostart entry of `app_name` under `home`, where the plugin
    /// reads and removes it.
    fn entry_path(app_name: &str, home: &Path) -> PathBuf {
        home.join(".config/autostart")
            .join(format!("{app_name}.desktop"))
    }

    /// The entry the plugin writes, naming `exec` with no arguments.
    pub fn entry(app_name: &str, exec: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nVersion=1.0\nName={app_name}\nComment={app_name} startup script\nExec={}\nStartupNotify=false\nTerminal=false\n",
            exec_value(&exec.to_string_lossy())
        )
    }

    /// `path` as the program of an `Exec` key: as it is when it holds only
    /// plain characters, else quoted as the Desktop Entry Specification
    /// says (`"`, `` ` ``, `$` and `\` escaped inside the quotes, `%`
    /// doubled), then with `\` escaped once more for the string value.
    fn exec_value(path: &str) -> String {
        let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+".contains(c);
        if path.chars().all(plain) {
            return path.to_owned();
        }
        let mut quoted = String::from("\"");
        for c in path.chars() {
            match c {
                '"' | '`' | '$' => {
                    quoted.push_str("\\\\");
                    quoted.push(c);
                }
                '\\' => quoted.push_str("\\\\\\\\"),
                '%' => quoted.push_str("%%"),
                c => quoted.push(c),
            }
        }
        quoted.push('"');
        quoted
    }

    /// Writes the autostart entry of `app_name` naming [`launcher_path`].
    /// With no such path it writes nothing, so launch at login stays off,
    /// and logs why; the switch then shows off again without an error.
    pub fn write_entry(app_name: &str) -> std::io::Result<()> {
        let Some(exec) = launcher_path() else {
            tracing::warn!(
                "Launch at login not turned on: no launcher of this build at a path that outlives an upgrade"
            );
            return Ok(());
        };
        let home = std::env::var_os("HOME")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"))?;
        write_entry_at(&entry_path(app_name, Path::new(&home)), app_name, &exec)
    }

    fn write_entry_at(path: &Path, app_name: &str, exec: &Path) -> std::io::Result<()> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        std::fs::write(path, entry(app_name, exec))
    }

    /// Whether an entry's `Exec` starts a program in the Nix store.
    fn execs_from_store(entry: &str) -> bool {
        entry
            .lines()
            .find_map(|line| line.strip_prefix("Exec="))
            .is_some_and(|exec| exec.trim_start().trim_start_matches('"').starts_with(STORE))
    }

    /// At launch while the system manages the login item: removes the
    /// autostart entry of `app_name` when its `Exec` starts a program in
    /// the Nix store (an earlier build wrote it), and only then.
    pub fn remove_store_entry(app_name: &str) {
        if let Some(home) = std::env::var_os("HOME") {
            remove_store_entry_at(&entry_path(app_name, Path::new(&home)));
        }
    }

    /// [`remove_store_entry`] at `path`; true when it removed the entry.
    fn remove_store_entry_at(path: &Path) -> bool {
        let Ok(entry) = std::fs::read_to_string(path) else {
            return false;
        };
        if !execs_from_store(&entry) {
            return false;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                tracing::info!(
                    "removed the autostart entry an earlier build wrote: the system starts Steno at login"
                );
                true
            }
            Err(error) => {
                tracing::warn!(%error, "the autostart entry an earlier build wrote could not be removed");
                false
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use super::*;

        /// A store with this build and an older one, each a wrapper beside
        /// the wrapped binary; the system profile links to the older build,
        /// the per-user profile to this one, and the user's profile is a
        /// linked tree whose `bin` is itself a link (as `buildEnv` makes
        /// for a single package).
        struct NixLayout {
            _dir: tempfile::TempDir,
            current_exe: PathBuf,
            system: PathBuf,
            per_user: PathBuf,
            user_profile: PathBuf,
        }

        fn build(store: &Path, name: &str) -> PathBuf {
            let bin = store.join(name).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join(BINARY), "#!/bin/sh\nexec .steno-desktop-wrapped\n").unwrap();
            std::fs::write(bin.join(".steno-desktop-wrapped"), "").unwrap();
            bin
        }

        fn nix_layout() -> NixLayout {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let store = root.join("nix/store");
            let this = build(&store, "aaaa-steno-desktop-0.12.0");
            let older = build(&store, "bbbb-steno-desktop-0.11.0");

            let system = root.join("run/current-system/sw/bin");
            std::fs::create_dir_all(&system).unwrap();
            symlink(older.join(BINARY), system.join(BINARY)).unwrap();

            let per_user = root.join("etc/profiles/per-user/ada/bin");
            std::fs::create_dir_all(&per_user).unwrap();
            symlink(this.join(BINARY), per_user.join(BINARY)).unwrap();

            let environment = store.join("cccc-user-environment");
            std::fs::create_dir_all(&environment).unwrap();
            symlink(&this, environment.join("bin")).unwrap();
            let home = root.join("home/ada");
            std::fs::create_dir_all(&home).unwrap();
            symlink(&environment, home.join(".nix-profile")).unwrap();

            NixLayout {
                current_exe: this.join(".steno-desktop-wrapped"),
                system: system.join(BINARY),
                per_user: per_user.join(BINARY),
                user_profile: home.join(".nix-profile/bin").join(BINARY),
                _dir: dir,
            }
        }

        #[test]
        fn the_candidates_are_usr_bin_then_the_nix_profiles() {
            let home = Path::new("/home/ada");
            assert_eq!(
                candidates(Some("ada"), Some(home)),
                [
                    PathBuf::from("/usr/bin/steno-desktop"),
                    PathBuf::from("/run/current-system/sw/bin/steno-desktop"),
                    PathBuf::from("/etc/profiles/per-user/ada/bin/steno-desktop"),
                    PathBuf::from("/home/ada/.nix-profile/bin/steno-desktop"),
                ]
            );
            assert_eq!(candidates(Some("../x"), None).len(), 2);
            assert_eq!(candidates(Some(""), None).len(), 2);
        }

        /// The wrapped binary in the store finds the profile that links to
        /// its own wrapper, skipping a profile that links to another build,
        /// and the path returned is the profile's, never the store's.
        #[test]
        fn a_nix_build_names_the_profile_that_links_to_it() {
            let layout = nix_layout();
            let found = stable_path(
                &[
                    layout.system.clone(),
                    layout.per_user.clone(),
                    layout.user_profile.clone(),
                ],
                &layout.current_exe,
            );
            assert_eq!(found.as_ref(), Some(&layout.per_user));
            assert!(!found.unwrap().to_string_lossy().contains("/nix/store/"));
        }

        /// A profile whose `bin` directory is itself a link counts too.
        #[test]
        fn a_profile_whose_bin_is_a_link_counts() {
            let layout = nix_layout();
            let found = stable_path(
                &[layout.system.clone(), layout.user_profile.clone()],
                &layout.current_exe,
            );
            assert_eq!(found, Some(layout.user_profile));
        }

        /// Only profiles that link to another build, or none at all: no
        /// path, so no entry.
        #[test]
        fn no_profile_of_this_build_means_no_path() {
            let layout = nix_layout();
            assert_eq!(
                stable_path(std::slice::from_ref(&layout.system), &layout.current_exe),
                None
            );
            assert_eq!(
                stable_path(
                    &[PathBuf::from("/nonexistent/steno-desktop")],
                    &layout.current_exe
                ),
                None
            );
        }

        /// The `.deb` and a package that installs the binary itself in
        /// `/usr/bin`: the candidate is the running binary.
        #[test]
        fn a_binary_in_place_names_itself() {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("usr/bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join(BINARY), "").unwrap();
            let elsewhere = dir.path().join("target/debug");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::fs::write(elsewhere.join(BINARY), "").unwrap();
            let candidate = [bin.join(BINARY)];
            assert_eq!(
                stable_path(&candidate, &bin.join(BINARY)),
                Some(bin.join(BINARY))
            );
            // A development build elsewhere names nothing.
            assert_eq!(stable_path(&candidate, &elsewhere.join(BINARY)), None);
        }

        #[test]
        fn a_named_path_is_an_absolute_file_outside_the_store() {
            let dir = tempfile::tempdir().unwrap();
            let launcher = dir.path().join(BINARY);
            std::fs::write(&launcher, "").unwrap();
            assert_eq!(named_path(launcher.as_os_str()), Some(launcher));
            assert_eq!(named_path(OsStr::new("steno-desktop")), None);
            assert_eq!(named_path(dir.path().as_os_str()), None);
            assert_eq!(
                named_path(OsStr::new(
                    "/nix/store/aaaa-steno-desktop/bin/steno-desktop"
                )),
                None
            );
            assert_eq!(named_path(OsStr::new("/nonexistent/steno-desktop")), None);
        }

        #[test]
        fn the_entry_names_the_path_and_quotes_what_needs_it() {
            let entry = entry("steno-desktop", Path::new("/usr/bin/steno-desktop"));
            assert!(entry.starts_with("[Desktop Entry]\n"));
            assert!(entry.contains("\nName=steno-desktop\n"));
            assert!(entry.contains("\nExec=/usr/bin/steno-desktop\n"));
            assert_eq!(
                exec_value("/home/a b/.nix-profile/bin/steno-desktop"),
                "\"/home/a b/.nix-profile/bin/steno-desktop\""
            );
            assert_eq!(exec_value("/x/100%/a$b"), "\"/x/100%%/a\\\\$b\"");
        }

        #[test]
        fn the_entry_is_written_where_the_plugin_reads_it() {
            let dir = tempfile::tempdir().unwrap();
            let path = entry_path("steno-desktop", dir.path());
            assert_eq!(
                path,
                dir.path().join(".config/autostart/steno-desktop.desktop")
            );
            write_entry_at(&path, "steno-desktop", Path::new("/usr/bin/steno-desktop")).unwrap();
            let written = std::fs::read_to_string(&path).unwrap();
            assert!(written.contains("\nExec=/usr/bin/steno-desktop\n"));
            assert!(!execs_from_store(&written));
        }

        /// While the system manages the login item, an entry that starts
        /// a program in the store goes; one that names a stable path, or
        /// any other text, stays.
        #[test]
        fn only_an_entry_into_the_store_is_removed() {
            let dir = tempfile::tempdir().unwrap();
            let path = entry_path("steno-desktop", dir.path());
            assert!(!remove_store_entry_at(&path), "no entry, nothing to do");

            let store = Path::new("/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped");
            write_entry_at(&path, "steno-desktop", store).unwrap();
            assert!(remove_store_entry_at(&path));
            assert!(!path.exists());

            std::fs::write(
                &path,
                "[Desktop Entry]\nExec=\"/nix/store/x/bin/steno-desktop\" \n",
            )
            .unwrap();
            assert!(remove_store_entry_at(&path));

            for stable in [
                "/usr/bin/steno-desktop",
                "/etc/profiles/per-user/ada/bin/steno-desktop",
            ] {
                write_entry_at(&path, "steno-desktop", Path::new(stable)).unwrap();
                assert!(!remove_store_entry_at(&path));
                assert!(path.exists());
            }
            std::fs::write(&path, "[Desktop Entry]\nName=/nix/store/x\n").unwrap();
            assert!(!remove_store_entry_at(&path));
            assert!(path.exists());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_managed_leaves_the_login_item_to_the_system() {
        assert!(managed(Some(OsStr::new("managed"))));
        assert!(!managed(None));
        assert!(!managed(Some(OsStr::new(""))));
        assert!(!managed(Some(OsStr::new("Managed"))));
        assert!(!managed(Some(OsStr::new("app"))));
    }
}
