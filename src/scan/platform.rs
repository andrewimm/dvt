//! Everything OS-specific about listing directories and reading sizes lives here.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::tree::Kind;

/// One directory entry as the platform reports it.
pub struct Raw {
    pub name: OsString,
    /// `None` if the entry couldn't be stat-ed.
    pub info: Option<Info>,
}

pub struct Info {
    pub kind: Kind,
    /// Allocated size, as `du` reports it.
    pub disk: u64,
    /// The device the entry is on, where the platform reports one per entry.
    pub dev: Option<u64>,
    /// A directory with another filesystem mounted on it, where the platform
    /// can tell mount points apart without comparing devices.
    pub mount_point: bool,
    /// (dev, ino) of a file with more than one link.
    pub link: Option<(u64, u64)>,
    /// A directory whose contents are in the cloud rather than on disk
    /// (iCloud Drive, File Provider); reading it would download them.
    pub dataless: bool,
}

/// Device of the scan root, compared against `Info::dev` for `-x`.
pub fn root_device(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path)
        .ok()
        .and_then(|m| imp::device(&m))
}

/// The scan root's own kind and size.
pub fn root_info(path: &Path) -> io::Result<(Kind, u64)> {
    let m = fs::symlink_metadata(path)?;
    Ok((kind_of(m.file_type()), imp::disk_usage(&m)))
}

/// Calls `f` with each entry of `dir`. Fails if the directory can't be read at
/// all; otherwise returns whether some entries couldn't be.
pub fn read_dir(dir: &Path, f: impl FnMut(Raw)) -> io::Result<bool> {
    imp::read_dir(dir, f)
}

/// An absolute path to `path` with symlinks resolved, spelled the way watch
/// events will report it.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    imp::canonicalize(path)
}

/// Process-wide settings for scanning, applied once at startup.
pub fn init() {
    imp::init()
}

fn kind_of(ft: fs::FileType) -> Kind {
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else if ft.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

/// Lists a directory with `std::fs`: one readdir and one fstatat per entry.
fn read_dir_std(dir: &Path, mut f: impl FnMut(Raw)) -> io::Result<bool> {
    let mut error = false;
    for de in fs::read_dir(dir)? {
        let Ok(de) = de else {
            error = true;
            continue;
        };
        // `DirEntry::metadata` does not follow symlinks (fstatat on unix).
        let info = de.metadata().ok().map(|m| {
            let kind = kind_of(m.file_type());
            Info {
                kind,
                disk: imp::disk_usage(&m),
                dev: imp::device(&m),
                mount_point: false,
                link: if kind == Kind::Dir {
                    None
                } else {
                    imp::hardlink_key(&m)
                },
                dataless: false,
            }
        });
        f(Raw {
            name: de.file_name(),
            info,
        });
    }
    Ok(error)
}

#[cfg(unix)]
mod unix {
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;

    /// Allocated size, as `du` reports it. `st_blocks` is always in 512-byte units.
    pub fn disk_usage(m: &Metadata) -> u64 {
        m.blocks() * 512
    }

    pub fn device(m: &Metadata) -> Option<u64> {
        Some(m.dev())
    }

    pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
        (m.nlink() > 1).then(|| (m.dev(), m.ino()))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use std::io;
    use std::path::{Path, PathBuf};

    pub use super::unix::*;
    use super::{Raw, read_dir_std};

    pub fn read_dir(dir: &Path, f: impl FnMut(Raw)) -> io::Result<bool> {
        read_dir_std(dir, f)
    }

    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    pub fn init() {}
}

/// macOS lists directories with `getattrlistbulk`, which returns names and
/// attributes for many entries per call instead of a stat per entry. It also
/// says which directories are mount points: on APFS the system and data
/// volumes report the same `st_dev`, so comparing devices can't tell that
/// `/System/Volumes/Data` is a second view of what `/Users`, `/Applications`
/// and the other firmlinks already show.
#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::ffi::{CString, OsStr, OsString};
    use std::io;
    use std::mem::size_of;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::ptr;

    pub use super::unix::*;
    use super::{Info, Raw, read_dir_std};
    use crate::tree::Kind;

    // Missing from the libc crate; values from <sys/attr.h>, <sys/stat.h>,
    // <sys/vnode.h> and <sys/resource.h>.
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const SF_DATALESS: u32 = 0x4000_0000;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
    const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;

    unsafe extern "C" {
        fn setiopolicy_np(
            iotype: libc::c_int,
            scope: libc::c_int,
            policy: libc::c_int,
        ) -> libc::c_int;
    }

    /// dvt is read-only, and that includes not downloading anything: with
    /// materialization off, touching a cloud-only file fails instead of
    /// fetching it.
    pub fn init() {
        // SAFETY: plain syscall wrapper; failure (macOS before 10.15) is harmless.
        unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            );
        }
    }

    /// `realpath` keeps a `/System/Volumes/Data/Users/...` path as it is, but
    /// FSEvents reports changes there under the firmlink (`/Users/...`), so
    /// such a root would never see an event. `F_GETPATH` on an open
    /// descriptor gives the firmlinked path.
    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        let real = std::fs::canonicalize(path)?;
        let Ok(fd) = open(&real, libc::O_RDONLY) else {
            return Ok(real);
        };
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a NUL-terminated path of at most PATH_MAX bytes.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } == -1 {
            return Ok(real);
        }
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(len);
        Ok(PathBuf::from(OsString::from_vec(buf)))
    }

    fn open(path: &Path, flags: libc::c_int) -> io::Result<OwnedFd> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `c` is a valid C string; the descriptor is owned on success.
        let fd = unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    const BUF_SIZE: usize = 128 * 1024;

    thread_local! {
        // u64 for alignment of the buffer start; entries inside are only
        // 4-byte aligned and are read unaligned.
        static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0; BUF_SIZE / 8]);
    }

    pub fn read_dir(dir: &Path, mut f: impl FnMut(Raw)) -> io::Result<bool> {
        let fd = open(dir, libc::O_RDONLY | libc::O_DIRECTORY)?;
        // Every entry that isn't a mount point is on the directory's device.
        // SAFETY: fstat fills `st` on success.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let dev = st.st_dev as u64;

        let mut attrs = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_RETURNED_ATTRS
                | libc::ATTR_CMN_NAME
                | ATTR_CMN_ERROR
                | libc::ATTR_CMN_OBJTYPE
                | libc::ATTR_CMN_FLAGS
                | libc::ATTR_CMN_FILEID,
            volattr: 0,
            dirattr: libc::ATTR_DIR_MOUNTSTATUS | libc::ATTR_DIR_ALLOCSIZE,
            fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE,
            forkattr: 0,
        };
        let mut error = false;
        let mut any = false;
        BUF.with_borrow_mut(|buf| {
            loop {
                // SAFETY: the buffer is BUF_SIZE bytes; the kernel fills at most that.
                let n = unsafe {
                    libc::getattrlistbulk(
                        fd.as_raw_fd(),
                        (&raw mut attrs).cast(),
                        buf.as_mut_ptr().cast(),
                        BUF_SIZE,
                        0,
                    )
                };
                if n < 0 {
                    let err = io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(libc::EINTR) => continue,
                        // A filesystem without bulk support: list it the slow way.
                        Some(libc::ENOTSUP | libc::EINVAL) if !any => {
                            return read_dir_std(dir, &mut f);
                        }
                        _ if !any => return Err(err),
                        _ => {
                            error = true;
                            break;
                        }
                    }
                }
                if n == 0 {
                    break;
                }
                any = true;
                let bytes: &[u8] =
                    // SAFETY: reinterpreting the initialised u64 buffer as bytes.
                    unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), BUF_SIZE) };
                let mut off = 0;
                for _ in 0..n {
                    let len = read::<u32>(bytes, off) as usize;
                    let raw = parse(&bytes[off..off + len], dir, dev);
                    error |= raw.info.is_none();
                    f(raw);
                    off += len;
                }
            }
            Ok(error)
        })
    }

    fn read<T: Copy>(b: &[u8], off: usize) -> T {
        assert!(off + size_of::<T>() <= b.len());
        // SAFETY: bounds checked above; attribute data is packed, so unaligned.
        unsafe { ptr::read_unaligned(b.as_ptr().add(off).cast()) }
    }

    /// Decodes one entry. Only attributes in the returned set are packed, in
    /// bit order within each group.
    fn parse(e: &[u8], dir: &Path, dev: u64) -> Raw {
        let mut p = size_of::<u32>();
        let ret: libc::attribute_set_t = read(e, p);
        p += size_of::<libc::attribute_set_t>();
        let has = |bit: u32| ret.commonattr & bit != 0;

        let failed = has(ATTR_CMN_ERROR) && read::<u32>(e, p) != 0;
        if has(ATTR_CMN_ERROR) {
            p += size_of::<u32>();
        }
        let mut name = OsString::new();
        if has(libc::ATTR_CMN_NAME) {
            let r: libc::attrreference_t = read(e, p);
            let start = p + r.attr_dataoffset as usize;
            // attr_length counts the terminating NUL.
            let end = start + (r.attr_length as usize).saturating_sub(1);
            name = OsStr::from_bytes(&e[start..end]).to_os_string();
            p += size_of::<libc::attrreference_t>();
        }
        let mut obj_type = None;
        if has(libc::ATTR_CMN_OBJTYPE) {
            obj_type = Some(read::<u32>(e, p));
            p += size_of::<u32>();
        }
        let mut st_flags = 0;
        if has(libc::ATTR_CMN_FLAGS) {
            st_flags = read::<u32>(e, p);
            p += size_of::<u32>();
        }
        let mut ino = None;
        if has(libc::ATTR_CMN_FILEID) {
            ino = Some(read::<u64>(e, p));
            p += size_of::<u64>();
        }
        let (Some(obj_type), false) = (obj_type, failed) else {
            return Raw { name, info: None };
        };

        let kind = match obj_type {
            VREG => Kind::File,
            VDIR => Kind::Dir,
            VLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        let mut info = Info {
            kind,
            disk: 0,
            dev: None,
            mount_point: false,
            link: None,
            dataless: kind == Kind::Dir && st_flags & SF_DATALESS != 0,
        };
        if ret.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
            info.mount_point = read::<u32>(e, p) & libc::DIR_MNTSTATUS_MNTPOINT != 0;
            p += size_of::<u32>();
        }
        if ret.dirattr & libc::ATTR_DIR_ALLOCSIZE != 0 {
            info.disk = read::<i64>(e, p).max(0) as u64;
            p += size_of::<i64>();
        }
        let mut nlink = 1;
        if ret.fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
            nlink = read::<u32>(e, p);
            p += size_of::<u32>();
        }
        if ret.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
            info.disk = read::<i64>(e, p).max(0) as u64;
        }
        if kind != Kind::Dir && nlink > 1 {
            info.link = ino.map(|ino| (dev, ino));
        }
        // The attributes describe the directory underneath the mount; the
        // size and device that matter are those of the mounted root.
        if info.mount_point
            && let Ok(m) = std::fs::symlink_metadata(dir.join(&name))
        {
            info.disk = disk_usage(&m);
        }
        Raw {
            name,
            info: Some(info),
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs::Metadata;
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{Raw, read_dir_std};

    // TODO(windows): use GetCompressedFileSizeW for allocated size and the
    // volume serial + file index for device and hardlink detection.
    pub fn disk_usage(m: &Metadata) -> u64 {
        m.len()
    }

    pub fn device(_: &Metadata) -> Option<u64> {
        None
    }

    pub fn hardlink_key(_: &Metadata) -> Option<(u64, u64)> {
        None
    }

    pub fn read_dir(dir: &Path, f: impl FnMut(Raw)) -> io::Result<bool> {
        read_dir_std(dir, f)
    }

    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    pub fn init() {}
}
