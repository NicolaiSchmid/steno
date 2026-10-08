//! The file system a folder is on, on Linux and macOS, for
//! [`super::may_lose_recent_writes`]: a server that acknowledges a flush
//! without writing it (Samba with `strict sync = no`, some NAS firmware)
//! can lose in a power cut a recording the durable writes synced, so
//! Settings warns about an audio folder on a network mount.
//!
//! `statfs` names the file system. On Linux its magic number decides
//! (`is_a_linux_network_file_system`). A FUSE mount shares one magic number
//! among local and remote file systems, so its type is read from
//! `/proc/self/mountinfo` (`mount_type`), and only the remote ones warn. On
//! macOS a mount without `MNT_LOCAL` warns, except an `autofs` trigger,
//! whose flags say nothing about what it mounts; the type name decides as
//! well (`is_a_macos_network_file_system`), because macFUSE reports
//! `macfuse` (or a name beginning with it), never whether it is remote, and
//! sets `MNT_LOCAL` for a mount made with `-o local`. macFUSE serves remote
//! file systems (sshfs, rclone, cloud drives) almost only, so every macFUSE
//! mount warns.

use std::path::Path;

/// Whether `path`, or its nearest ancestor that exists, is on a network
/// mount; false where no part of it exists or its file system cannot be
/// read. A FUSE mount whose server has gone (`ENOTCONN`) reads as missing,
/// so the folder it is mounted on decides until it is back; writes into it
/// fail meanwhile.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn is_on_a_network_mount(path: &Path) -> bool {
    let Some(existing) = nearest_existing(path) else {
        return false;
    };
    let stat = match rustix::fs::statfs(existing) {
        Ok(stat) => stat,
        Err(error) => {
            tracing::debug!(folder = %existing.display(), %error, "the folder's file system could not be read");
            return false;
        }
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
        is_a_linux_network_file_system(magic, || mount_type_of(existing))
    }
    #[cfg(target_os = "macos")]
    {
        is_a_macos_network_file_system(&type_name(&stat.f_fstypename), stat.f_flags)
    }
}

/// `path`, or its nearest ancestor that exists.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn nearest_existing(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.exists())
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
/// `CEPH_SUPER_MAGIC`, the kernel Ceph client.
#[cfg(any(target_os = "linux", test))]
const CEPH_SUPER_MAGIC: u32 = 0x00C3_6400;
/// `AFS_SUPER_MAGIC`, the `OpenAFS` client.
#[cfg(any(target_os = "linux", test))]
const AFS_SUPER_MAGIC: u32 = 0x5346_414F;
/// `AFS_FS_MAGIC`, the kernel's own AFS client (kAFS).
#[cfg(any(target_os = "linux", test))]
const AFS_FS_MAGIC: u32 = 0x6B41_4653;
/// `V9FS_MAGIC`, 9P (a share from a VM host or another machine).
#[cfg(any(target_os = "linux", test))]
const V9FS_MAGIC: u32 = 0x0102_1997;
/// `LL_SUPER_MAGIC`, Lustre.
#[cfg(any(target_os = "linux", test))]
const LUSTRE_SUPER_MAGIC: u32 = 0x0BD0_0BD0;
/// `GPFS_SUPER_MAGIC`, IBM Storage Scale.
#[cfg(any(target_os = "linux", test))]
const GPFS_SUPER_MAGIC: u32 = 0x4750_4653;
/// `CODA_SUPER_MAGIC`.
#[cfg(any(target_os = "linux", test))]
const CODA_SUPER_MAGIC: u32 = 0x7375_7245;
/// `NCP_SUPER_MAGIC`, the `NetWare` client.
#[cfg(any(target_os = "linux", test))]
const NCP_SUPER_MAGIC: u32 = 0x564C;
/// `ORANGEFS_SUPER_MAGIC`.
#[cfg(any(target_os = "linux", test))]
const ORANGEFS_SUPER_MAGIC: u32 = 0x2003_0528;
/// `FUSE_SUPER_MAGIC`, every FUSE file system (`fuse` and `fuseblk`).
#[cfg(any(target_os = "linux", test))]
const FUSE_SUPER_MAGIC: u32 = 0x6573_5546;

/// The magic numbers of the Linux network file systems outside FUSE.
#[cfg(any(target_os = "linux", test))]
const NETWORK_MAGIC_NUMBERS: [u32; 13] = [
    NFS_SUPER_MAGIC,
    SMB_SUPER_MAGIC,
    CIFS_MAGIC_NUMBER,
    SMB2_MAGIC_NUMBER,
    CEPH_SUPER_MAGIC,
    AFS_SUPER_MAGIC,
    AFS_FS_MAGIC,
    V9FS_MAGIC,
    LUSTRE_SUPER_MAGIC,
    GPFS_SUPER_MAGIC,
    CODA_SUPER_MAGIC,
    NCP_SUPER_MAGIC,
    ORANGEFS_SUPER_MAGIC,
];

/// The FUSE types in `/proc/self/mountinfo` that reach another machine:
/// sshfs, rclone, GNOME's `gvfsd-fuse` and KDE's `kio-fuse`, under which
/// the file managers mount SMB, SFTP and `WebDAV` shares, the Gluster and
/// Ceph clients, the S3 and Cloud Storage mounts (s3fs, gcsfuse,
/// mountpoint-s3, `JuiceFS`), curlftpfs and smbnetfs.
#[cfg(any(target_os = "linux", test))]
const REMOTE_FUSE_TYPES: [&str; 12] = [
    "fuse.sshfs",
    "fuse.rclone",
    "fuse.gvfsd-fuse",
    "fuse.kio-fuse",
    "fuse.glusterfs",
    "fuse.ceph-fuse",
    "fuse.s3fs",
    "fuse.gcsfuse",
    "fuse.mountpoint-s3",
    "fuse.juicefs",
    "fuse.curlftpfs",
    "fuse.smbnetfs",
];

/// Whether the Linux file system with the `statfs` magic number `magic` is
/// a network mount: one of [`NETWORK_MAGIC_NUMBERS`], or a FUSE mount whose
/// type (`fuse_type`, read only for FUSE) is one of [`REMOTE_FUSE_TYPES`].
#[cfg(any(target_os = "linux", test))]
fn is_a_linux_network_file_system(magic: u32, fuse_type: impl FnOnce() -> Option<String>) -> bool {
    if magic == FUSE_SUPER_MAGIC {
        fuse_type().is_some_and(|name| REMOTE_FUSE_TYPES.contains(&name.as_str()))
    } else {
        NETWORK_MAGIC_NUMBERS.contains(&magic)
    }
}

/// The type `/proc/self/mountinfo` names for the mount `folder` is on,
/// found by the folder's device number.
#[cfg(target_os = "linux")]
fn mount_type_of(folder: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let read = || -> std::io::Result<_> {
        let device = std::fs::metadata(folder)?.dev();
        Ok((device, std::fs::read_to_string("/proc/self/mountinfo")?))
    };
    let (device, mountinfo) = read()
        .inspect_err(|error| {
            tracing::debug!(folder = %folder.display(), %error, "the folder's FUSE type could not be read");
        })
        .ok()?;
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

/// `MNT_LOCAL`, the `statfs` flag of a macOS file system on a local device.
#[cfg(any(target_os = "macos", test))]
const MNT_LOCAL: u32 = 0x1000;

/// Whether the macOS file system with the type name `name` and the mount
/// flags `flags` (`statfs`'s `f_fstypename` and `f_flags`) is a network
/// mount: one without [`MNT_LOCAL`] other than an `autofs` trigger, and,
/// whatever its flags, SMB, NFS, AFP, `WebDAV` and every macFUSE mount
/// (`macfuse`, `osxfuse` before version 4, with or without a suffix).
#[cfg(any(target_os = "macos", test))]
fn is_a_macos_network_file_system(name: &str, flags: u32) -> bool {
    ((flags & MNT_LOCAL) == 0 && name != "autofs")
        || matches!(name, "smbfs" | "nfs" | "afpfs" | "webdav")
        || name.starts_with("macfuse")
        || name.starts_with("osxfuse")
}

/// `f_fstypename`, a NUL-terminated name in a fixed buffer, as a string.
#[cfg(target_os = "macos")]
fn type_name(name: &[std::ffi::c_char]) -> String {
    let bytes: Vec<u8> = name
        .iter()
        .take_while(|&&unit| unit != 0)
        .map(|&unit| unit.cast_unsigned())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing folder is read through its nearest ancestor that exists.
    #[test]
    fn a_missing_folder_resolves_to_its_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(nearest_existing(dir.path()), Some(dir.path()));
        let deeper = dir.path().join("missing").join("deeper");
        assert_eq!(nearest_existing(&deeper), Some(dir.path()));
    }

    /// NFS, the three SMB clients and the other network file systems warn;
    /// local file systems do not.
    #[test]
    fn linux_nfs_and_smb_warn_and_local_file_systems_do_not() {
        let unread = || -> Option<String> { panic!("read the type of a mount that is not FUSE") };
        for magic in [
            NFS_SUPER_MAGIC,
            SMB_SUPER_MAGIC,
            CIFS_MAGIC_NUMBER,
            SMB2_MAGIC_NUMBER,
            CEPH_SUPER_MAGIC,
            AFS_SUPER_MAGIC,
            AFS_FS_MAGIC,
            V9FS_MAGIC,
            LUSTRE_SUPER_MAGIC,
            GPFS_SUPER_MAGIC,
            CODA_SUPER_MAGIC,
            NCP_SUPER_MAGIC,
            ORANGEFS_SUPER_MAGIC,
        ] {
            assert!(is_a_linux_network_file_system(magic, unread), "{magic:#x}");
        }
        // ext4, btrfs, xfs, tmpfs, overlayfs, and balloon-kvm-fs, whose
        // number has been mistaken for OrangeFS's.
        for magic in [
            0xEF53,
            0x9123_683E,
            0x5846_5342,
            0x0102_1994,
            0x794C_7630,
            0x1366_1366,
        ] {
            assert!(!is_a_linux_network_file_system(magic, unread), "{magic:#x}");
        }
    }

    /// A FUSE mount warns only when its type is a remote one, and not when
    /// the type cannot be read.
    #[test]
    fn a_linux_fuse_mount_warns_only_when_its_type_is_remote() {
        for name in [
            "fuse.sshfs",
            "fuse.rclone",
            "fuse.gvfsd-fuse",
            "fuse.kio-fuse",
            "fuse.glusterfs",
            "fuse.ceph-fuse",
            "fuse.s3fs",
            "fuse.gcsfuse",
            "fuse.mountpoint-s3",
            "fuse.juicefs",
            "fuse.curlftpfs",
            "fuse.smbnetfs",
        ] {
            assert!(
                is_a_linux_network_file_system(FUSE_SUPER_MAGIC, || Some(name.to_owned())),
                "{name}"
            );
        }
        for name in ["fuseblk", "fuse.portal", "fuse.appimage", "fuse"] {
            assert!(
                !is_a_linux_network_file_system(FUSE_SUPER_MAGIC, || Some(name.to_owned())),
                "{name}"
            );
        }
        assert!(!is_a_linux_network_file_system(FUSE_SUPER_MAGIC, || None));
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
        assert_eq!(mount_type_of(Path::new("/proc")).as_deref(), Some("proc"));
    }

    /// SMB, NFS, AFP, `WebDAV` and macFUSE warn on macOS even with
    /// `MNT_LOCAL` set; APFS, HFS+, exFAT, FAT and devfs with it do not.
    #[test]
    fn macos_network_file_system_names_warn() {
        for name in [
            "smbfs",
            "nfs",
            "afpfs",
            "webdav",
            "macfuse",
            "osxfuse",
            "macfuse_sshfs",
        ] {
            assert!(is_a_macos_network_file_system(name, MNT_LOCAL), "{name}");
        }
        for name in ["apfs", "hfs", "exfat", "msdos", "devfs", ""] {
            assert!(!is_a_macos_network_file_system(name, MNT_LOCAL), "{name}");
        }
    }

    /// A macOS mount without `MNT_LOCAL` warns whatever its name, except an
    /// `autofs` trigger (flags as read on a Mac).
    #[test]
    fn a_macos_mount_without_mnt_local_warns_except_autofs() {
        let trigger = 0x0450_0000;
        assert_eq!(trigger & MNT_LOCAL, 0);
        assert!(!is_a_macos_network_file_system("autofs", trigger));
        for name in ["apfs", "unlisted"] {
            assert!(is_a_macos_network_file_system(name, trigger), "{name}");
        }
        // APFS as read on the same host: `MNT_LOCAL` set.
        assert!(!is_a_macos_network_file_system("apfs", 0x4480_D001));
    }

    /// The NUL ends the type name in its buffer.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_type_name_ends_at_the_nul() {
        let mut buffer = [0 as std::ffi::c_char; 16];
        for (unit, byte) in buffer.iter_mut().zip(b"smbfs") {
            *unit = byte.cast_signed();
        }
        assert_eq!(type_name(&buffer), "smbfs");
    }
}
