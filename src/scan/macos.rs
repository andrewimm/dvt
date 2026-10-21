//! The macOS directory reader: `getattrlistbulk` into a reused per-thread
//! buffer, which returns names and attributes for many entries per call
//! instead of a stat per entry.
//!
//! It also says which directories are mount points. On APFS the system and
//! data volumes report the same `st_dev`, so comparing devices can't tell that
//! `/System/Volumes/Data` is a second view of what `/Users`, `/Applications`
//! and the other firmlinks already show; `-x` goes by mount points instead.

use std::cell::RefCell;
use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::mem::size_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr;

use super::{Ctx, Listing, Parent, Stat, classify, read_dir_std};
use crate::tree::{Kind, flags};

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
    fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
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
/// FSEvents reports changes there under the firmlink (`/Users/...`), so such
/// a root would never see an event. `F_GETPATH` on an open descriptor gives
/// the firmlinked path.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    let real = std::fs::canonicalize(path)?;
    let Ok(fd) = open_at(libc::AT_FDCWD, real.as_os_str(), libc::O_RDONLY) else {
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

fn open_at(dirfd: libc::c_int, path: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let c = CString::new(path.as_bytes())?;
    // SAFETY: `c` is a valid C string; the descriptor is owned on success.
    let fd = unsafe { libc::openat(dirfd, c.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

const BUF_SIZE: usize = 128 * 1024;

thread_local! {
    // u64 for alignment of the buffer start; entries inside are only 4-byte
    // aligned and are read unaligned.
    static BUF: RefCell<Vec<u64>> = RefCell::new(vec![0; BUF_SIZE / 8]);
}

/// Reads `path`, opening it relative to `parent` when that's available.
/// Returns the open directory too, so its own children can do the same.
pub(super) fn read_dir(
    path: &Path,
    parent: Option<&Parent>,
    ctx: &Ctx,
) -> (Listing, bool, Option<OwnedFd>) {
    let dir_flags = libc::O_RDONLY | libc::O_DIRECTORY;
    let fd = match (parent, path.file_name()) {
        (Some(p), Some(name)) => open_at(p.fd.as_raw_fd(), name, dir_flags | libc::O_NOFOLLOW),
        _ => open_at(libc::AT_FDCWD, path.as_os_str(), dir_flags),
    };
    let Ok(fd) = fd else {
        return (Listing::default(), true, None);
    };
    match BUF.with_borrow_mut(|buf| read_fd(&fd, path, buf, ctx)) {
        Ok((listing, error)) => (listing, error, Some(fd)),
        // A filesystem without bulk support: list it the portable way.
        Err(()) => {
            let (listing, error, _) = read_dir_std(path, None, ctx);
            (listing, error, Some(fd))
        }
    }
}

/// Fails only if the filesystem doesn't support `getattrlistbulk`.
fn read_fd(fd: &OwnedFd, path: &Path, buf: &mut [u64], ctx: &Ctx) -> Result<(Listing, bool), ()> {
    let mut listing = Listing::default();
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
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
            match io::Error::last_os_error().raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ENOTSUP | libc::EINVAL) if !any => return Err(()),
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
        // SAFETY: reinterpreting the initialised u64 buffer as bytes.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), BUF_SIZE) };
        let mut off = 0;
        for _ in 0..n {
            let len = read::<u32>(bytes, off) as usize;
            parse(&bytes[off..off + len], path, ctx, &mut listing);
            off += len;
        }
    }
    Ok((listing, error))
}

fn read<T: Copy>(b: &[u8], off: usize) -> T {
    assert!(off + size_of::<T>() <= b.len());
    // SAFETY: bounds checked above; attribute data is packed, so unaligned.
    unsafe { ptr::read_unaligned(b.as_ptr().add(off).cast()) }
}

/// Decodes one entry into `listing`. Only attributes in the returned set are
/// packed, in bit order within each group.
fn parse(e: &[u8], dir: &Path, ctx: &Ctx, listing: &mut Listing) {
    let mut p = size_of::<u32>();
    let ret: libc::attribute_set_t = read(e, p);
    p += size_of::<libc::attribute_set_t>();
    let has = |bit: u32| ret.commonattr & bit != 0;

    let failed = has(ATTR_CMN_ERROR) && read::<u32>(e, p) != 0;
    if has(ATTR_CMN_ERROR) {
        p += size_of::<u32>();
    }
    let mut name: &OsStr = OsStr::new("");
    if has(libc::ATTR_CMN_NAME) {
        let r: libc::attrreference_t = read(e, p);
        let start = p + r.attr_dataoffset as usize;
        // attr_length counts the terminating NUL.
        let end = start + (r.attr_length as usize).saturating_sub(1);
        name = OsStr::from_bytes(&e[start..end]);
        p += size_of::<libc::attrreference_t>();
    }
    // For hardlink keys; mount points are told apart by their mount status.
    let mut dev = 0;
    if has(libc::ATTR_CMN_DEVID) {
        dev = read::<libc::dev_t>(e, p) as u64;
        p += size_of::<libc::dev_t>();
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
    let mut ino = 0;
    if has(libc::ATTR_CMN_FILEID) {
        ino = read::<u64>(e, p);
        p += size_of::<u64>();
    }
    let (Some(obj_type), false) = (obj_type, failed) else {
        listing.push(name, Kind::Other, 0, flags::ERROR);
        return;
    };
    let kind = match obj_type {
        VREG => Kind::File,
        VDIR => Kind::Dir,
        VLNK => Kind::Symlink,
        _ => Kind::Other,
    };
    if !ctx.opts.excludes.is_empty() && ctx.opts.excludes.matches(dir, name, kind == Kind::Dir) {
        let kind = if kind == Kind::Dir {
            Kind::Dir
        } else {
            Kind::File
        };
        listing.push(name, kind, 0, flags::EXCLUDED);
        return;
    }

    let mut mount_point = false;
    let mut disk = 0;
    if ret.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        mount_point = read::<u32>(e, p) & libc::DIR_MNTSTATUS_MNTPOINT != 0;
        p += size_of::<u32>();
    }
    if ret.dirattr & libc::ATTR_DIR_ALLOCSIZE != 0 {
        disk = read::<i64>(e, p).max(0) as u64;
        p += size_of::<i64>();
    }
    let mut nlink = 1;
    if ret.fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
        nlink = read::<u32>(e, p);
        p += size_of::<u32>();
    }
    if ret.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
        disk = read::<i64>(e, p).max(0) as u64;
    }
    // The attributes describe the directory underneath the mount; the size
    // that matters is the mounted root's.
    if mount_point && let Ok(m) = std::fs::symlink_metadata(dir.join(name)) {
        disk = super::platform::disk_usage(&m);
    }
    let stat = Stat {
        kind,
        disk,
        dev,
        ino,
        nlink: nlink.into(),
        mount_point: Some(mount_point),
        dataless: kind == Kind::Dir && st_flags & SF_DATALESS != 0,
    };
    let (kind, disk, fl) = classify(ctx, stat);
    listing.push(name, kind, disk, fl);
}
