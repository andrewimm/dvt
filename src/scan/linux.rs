//! The Linux directory reader: `getdents64` into a reused per-thread buffer
//! and `statx` on the names in place, so listing a directory allocates
//! nothing per entry beyond the entry itself.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, FileType, Mode, OFlags, RawDir, StatxFlags, makedev, openat, statx,
};

use super::{Ctx, Listing, Parent, Stat, classify};
use crate::tree::{Kind, flags};

/// Big enough for hundreds of entries per `getdents64` call.
const BUF_SIZE: usize = 64 * 1024;

thread_local! {
    static BUF: RefCell<Vec<MaybeUninit<u8>>> = RefCell::new(vec![MaybeUninit::uninit(); BUF_SIZE]);
}

/// Only what dvt uses: no times, owners or permissions.
const MASK: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::NLINK)
    .union(StatxFlags::INO)
    .union(StatxFlags::BLOCKS);

pub(super) fn open_dir(path: &Path) -> rustix::io::Result<OwnedFd> {
    openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

/// Reads `path`, opening it relative to `parent` when that's available.
/// Returns the open directory too, so its own children can do the same.
pub(super) fn read_dir(
    path: &Path,
    parent: Option<&Parent>,
    ctx: &Ctx,
) -> (Listing, bool, Option<OwnedFd>) {
    let fd = match (parent, path.file_name()) {
        (Some(p), Some(name)) => openat(
            &p.fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ),
        _ => open_dir(path),
    };
    let Ok(fd) = fd else {
        return (Listing::default(), true, None);
    };
    let (listing, error) = BUF.with_borrow_mut(|buf| read_fd(&fd, path, buf, ctx));
    (listing, error, Some(fd))
}

fn read_fd(fd: &OwnedFd, path: &Path, buf: &mut [MaybeUninit<u8>], ctx: &Ctx) -> (Listing, bool) {
    let mut listing = Listing::default();
    let mut error = false;
    let mut dir = RawDir::new(fd, buf);
    while let Some(next) = dir.next() {
        let Ok(de) = next else {
            error = true;
            break;
        };
        let cname = de.file_name();
        let bytes = cname.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsStr::from_bytes(bytes);
        // The type from getdents is free; an excluded entry needs nothing more.
        if !ctx.opts.excludes.is_empty() {
            let is_dir = de.file_type() == FileType::Directory;
            if ctx.opts.excludes.matches(path, name, is_dir) {
                let kind = if is_dir { Kind::Dir } else { Kind::File };
                listing.push(name, kind, 0, flags::EXCLUDED);
                continue;
            }
        }
        // DONT_SYNC: network filesystems may answer from cache.
        let st = statx(
            fd,
            cname,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC,
            MASK,
        );
        let Ok(st) = st else {
            listing.push(name, Kind::Other, 0, flags::ERROR);
            continue;
        };
        let stat = Stat {
            kind: kind_of(FileType::from_raw_mode(st.stx_mode.into())),
            disk: st.stx_blocks * 512,
            dev: makedev(st.stx_dev_major, st.stx_dev_minor),
            ino: st.stx_ino,
            nlink: st.stx_nlink.into(),
            mount_point: None,
            dataless: false,
        };
        let (kind, disk, fl) = classify(ctx, stat);
        listing.push(name, kind, disk, fl);
    }
    (listing, error)
}

fn kind_of(ft: FileType) -> Kind {
    match ft {
        FileType::Directory => Kind::Dir,
        FileType::RegularFile => Kind::File,
        FileType::Symlink => Kind::Symlink,
        _ => Kind::Other,
    }
}
