//! Replacing a file in one step, for the secrets file and the CLI's
//! `meeting.json`.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

/// Replaces `path` with `data`: the bytes go to `temporary`, are synced,
/// and a rename puts them in place, so a reader, a crash or a full disk
/// sees the old file or the new one, never a torn one. A failure removes
/// `temporary`; one a killed process left behind is overwritten. With
/// `owner_only` the file is 0600 where the platform has modes, even when
/// a stale `temporary` had another mode.
pub fn replace_file(
    path: &Path,
    temporary: &Path,
    data: &[u8],
    owner_only: bool,
) -> std::io::Result<()> {
    let written = (|| {
        let mut options = OpenOptions::new();
        options.create(true).write(true).truncate(true);
        if owner_only {
            restrict_new_file(&mut options);
        }
        let mut file = options.open(temporary)?;
        if owner_only {
            restrict_to_owner(temporary)?;
        }
        file.write_all(data)?;
        file.sync_all()?;
        std::fs::rename(temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    written?;
    sync_directory(path.parent());
    Ok(())
}

/// Makes `options` create the file with mode 0600 where the platform has
/// modes.
pub fn restrict_new_file(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// Sets mode 0600 on `path` where the platform has modes.
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Makes a rename in `directory` durable where the platform can sync a
/// directory; best effort.
fn sync_directory(directory: Option<&Path>) {
    #[cfg(unix)]
    if let Some(directory) = directory
        && let Ok(handle) = std::fs::File::open(directory)
    {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}
