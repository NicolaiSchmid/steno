//! The file system a folder is on, on Linux and macOS, for
//! [`super::may_lose_recent_writes`]: a server that acknowledges a flush
//! without writing it (Samba with `strict sync = no`, some NAS firmware)
//! can lose in a power cut a recording the durable writes synced, so
//! Settings warns about an audio folder on a network mount.
//!
//! `statfs` names the file system: on Linux by its magic number
//! (`is_a_network_file_system`), on macOS by its type name
//! (`is_a_network_file_system_name`). A FUSE mount on Linux shares one
//! magic number among local and remote file systems, so its type is read
//! from `/proc/self/mountinfo` (`mount_type`), and only the remote ones
//! warn. On macOS FUSE reports one type name for all its file systems, and
//! there it serves remote ones (sshfs, rclone, cloud drives) almost only,
//! so every macFUSE mount warns.

/// Whether the nearest existing folder of `path` is on a network mount;
/// false where no part of it exists or its file system cannot be read.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn is_on_a_network_mount(path: &std::path::Path) -> bool {
    let Some(existing) = path.ancestors().find(|ancestor| ancestor.exists()) else {
        return false;
    };
    let Ok(stat) = rustix::fs::statfs(existing) else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        // The magic numbers are 32 bits; where `f_type` is a signed 32-bit
        // word the cast keeps the bits of the ones above `i32::MAX`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::unnecessary_cast,
            reason = "f_type's width and sign differ between targets"
        )]
        let magic = stat.f_type as u32;
        is_a_network_file_system(magic, || mount_type_of(existing))
    }
    #[cfg(target_os = "macos")]
    {
        is_a_network_file_system_name(&type_name(&stat.f_fstypename))
    }
}

/// `NFS_SUPER_MAGIC`.
#[cfg(any(target_os = "linux", test))]
const NFS_SUPER_MAGIC: u32 = 0x6969;
/// `SMB_SUPER_MAGIC`, the old `smbfs`.
#[cfg(any(target_os = "linux", test))]
const SMB_SUPER_MAGIC: u32 = 0x517B;
/// `CIFS_MAGIC_NUMBER`, the `cifs` client.
#[cfg(any(target_os = "linux", test))]
const CIFS_MAGIC_NUMBER: u32 = 0xFF53_4D42;
/// `SMB2_MAGIC_NUMBER`, the `cifs` client mounted as `smb3`.
#[cfg(any(target_os = "linux", test))]
const SMB2_MAGIC_NUMBER: u32 = 0xFE53_4D42;
/// `FUSE_SUPER_MAGIC`, every FUSE file system (`fuse` and `fuseblk`).
#[cfg(any(target_os = "linux", test))]
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// The FUSE types in `/proc/self/mountinfo` that reach another machine:
/// sshfs, rclone, and GNOME's `gvfsd-fuse`, under which the file manager
/// mounts SMB, SFTP and `WebDAV` shares.
#[cfg(any(target_os = "linux", test))]
const REMOTE_FUSE_TYPES: [&str; 3] = ["fuse.sshfs", "fuse.rclone", "fuse.gvfsd-fuse"];

/// Whether the Linux file system with the `statfs` magic number `magic` is
/// a network mount: NFS and SMB, and a FUSE mount whose type
/// (`fuse_type`, read only for FUSE) is a remote one.
#[cfg(any(target_os = "linux", test))]
fn is_a_network_file_system(magic: u32, fuse_type: impl FnOnce() -> Option<String>) -> bool {
    match magic {
        NFS_SUPER_MAGIC | SMB_SUPER_MAGIC | CIFS_MAGIC_NUMBER | SMB2_MAGIC_NUMBER => true,
        FUSE_SUPER_MAGIC => {
            fuse_type().is_some_and(|name| REMOTE_FUSE_TYPES.contains(&name.as_str()))
        }
        _ => false,
    }
}

/// The type `/proc/self/mountinfo` names for the mount `folder` is on,
/// found by the folder's device number.
#[cfg(target_os = "linux")]
fn mount_type_of(folder: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let device = std::fs::metadata(folder).ok()?.dev();
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    mount_type(
        &mountinfo,
        rustix::fs::major(device),
        rustix::fs::minor(device),
    )
    .map(str::to_owned)
}

/// The file system type of the mount of device `major:minor` in
/// `mountinfo`, whose lines read `<id> <parent> <major:minor> <root>
/// <mount point> <options> [optional fields] - <type> <source> <options>`.
/// The fields before the ` - ` escape their spaces, so the first ` - `
/// ends them.
#[cfg(any(target_os = "linux", test))]
fn mount_type(mountinfo: &str, major: u32, minor: u32) -> Option<&str> {
    let device = format!("{major}:{minor}");
    mountinfo.lines().find_map(|line| {
        let (mount, file_system) = line.split_once(" - ")?;
        if mount.split(' ').nth(2)? != device {
            return None;
        }
        file_system.split(' ').next()
    })
}

/// Whether the macOS file system type `name` (`statfs`'s `f_fstypename`)
/// is a network mount: SMB, NFS, AFP, `WebDAV`, and every macFUSE mount
/// (`macfuse`, `osxfuse` before version 4, with or without a suffix).
#[cfg(any(target_os = "macos", test))]
fn is_a_network_file_system_name(name: &str) -> bool {
    matches!(name, "smbfs" | "nfs" | "afpfs" | "webdav")
        || name.starts_with("macfuse")
        || name.starts_with("osxfuse")
}

/// `f_fstypename`, a NUL-terminated name in a fixed buffer, as a string.
#[cfg(target_os = "macos")]
fn type_name(name: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = name
        .iter()
        .take_while(|&&unit| unit != 0)
        .map(|&unit| u8::from_ne_bytes(unit.to_ne_bytes()))
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NFS and the three SMB clients warn; local file systems do not.
    #[test]
    fn nfs_and_smb_are_network_file_systems_and_local_ones_are_not() {
        let unread = || -> Option<String> { panic!("read the type of a mount that is not FUSE") };
        for magic in [
            NFS_SUPER_MAGIC,
            SMB_SUPER_MAGIC,
            CIFS_MAGIC_NUMBER,
            SMB2_MAGIC_NUMBER,
        ] {
            assert!(is_a_network_file_system(magic, unread), "{magic:#x}");
        }
        // ext4, btrfs, xfs, tmpfs, overlayfs.
        for magic in [0xEF53, 0x9123_683E, 0x5846_5342, 0x0102_1994, 0x794C_7630] {
            assert!(!is_a_network_file_system(magic, unread), "{magic:#x}");
        }
    }

    /// A FUSE mount warns only when its type is a remote one, and not when
    /// the type cannot be read.
    #[test]
    fn a_fuse_mount_is_a_network_file_system_only_when_its_type_is_remote() {
        for name in ["fuse.sshfs", "fuse.rclone", "fuse.gvfsd-fuse"] {
            assert!(
                is_a_network_file_system(FUSE_SUPER_MAGIC, || Some(name.to_owned())),
                "{name}"
            );
        }
        for name in ["fuseblk", "fuse.portal", "fuse.appimage", "fuse"] {
            assert!(
                !is_a_network_file_system(FUSE_SUPER_MAGIC, || Some(name.to_owned())),
                "{name}"
            );
        }
        assert!(!is_a_network_file_system(FUSE_SUPER_MAGIC, || None));
    }

    /// The type is found by the device number, after the optional fields,
    /// in a mount point with an escaped space.
    #[test]
    fn the_mount_type_is_read_by_device_number() {
        let mountinfo = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
45 22 0:41 / /home/me/NAS\\040share rw,nosuid,nodev,relatime shared:30 master:2 - fuse.sshfs me@nas:/srv rw,user_id=1000
46 22 0:42 / /mnt/backup rw,relatime - nfs4 nas:/backup rw,vers=4.2";
        assert_eq!(mount_type(mountinfo, 259, 2), Some("ext4"));
        assert_eq!(mount_type(mountinfo, 0, 41), Some("fuse.sshfs"));
        assert_eq!(mount_type(mountinfo, 0, 42), Some("nfs4"));
        assert_eq!(mount_type(mountinfo, 0, 4), None);
    }

    /// The kernel's mountinfo is keyed as `mount_type_of` reads it:
    /// `/proc` is found as `proc`.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_mount_type_of_proc_is_proc() {
        assert_eq!(
            mount_type_of(std::path::Path::new("/proc")).as_deref(),
            Some("proc")
        );
    }

    /// SMB, NFS, AFP, `WebDAV` and macFUSE warn on macOS; APFS, HFS+, exFAT
    /// and FAT do not.
    #[test]
    fn macos_network_file_systems_are_named() {
        for name in [
            "smbfs",
            "nfs",
            "afpfs",
            "webdav",
            "macfuse",
            "osxfuse",
            "macfuse_sshfs",
        ] {
            assert!(is_a_network_file_system_name(name), "{name}");
        }
        for name in ["apfs", "hfs", "exfat", "msdos", "devfs", ""] {
            assert!(!is_a_network_file_system_name(name), "{name}");
        }
    }

    /// The NUL ends the type name in its buffer.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_type_name_ends_at_the_nul() {
        let mut buffer = [0 as std::ffi::c_char; 16];
        for (unit, byte) in buffer.iter_mut().zip(b"smbfs") {
            *unit = std::ffi::c_char::from_ne_bytes(byte.to_ne_bytes());
        }
        assert_eq!(type_name(&buffer), "smbfs");
    }

    /// A local test folder, and a missing folder in it, are not on a
    /// network mount.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_local_folder_is_not_on_a_network_mount() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!is_on_a_network_mount(directory.path()));
        assert!(!is_on_a_network_mount(&directory.path().join("audio")));
    }
}
