//! Everything OS-specific about reading sizes lives here.

use std::fs::Metadata;

#[cfg(unix)]
mod imp {
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;

    /// Allocated size, as `du` reports it. `st_blocks` is always in 512-byte units.
    pub fn disk_usage(m: &Metadata) -> u64 {
        m.blocks() * 512
    }

    pub fn device(m: &Metadata) -> Option<u64> {
        Some(m.dev())
    }

    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub fn identity(m: &Metadata) -> (u64, u64, u64) {
        (m.dev(), m.ino(), m.nlink())
    }
}

#[cfg(not(unix))]
mod imp {
    use std::fs::Metadata;

    // TODO(windows): use GetCompressedFileSizeW for allocated size and the
    // volume serial + file index for device and hardlink detection.
    pub fn disk_usage(m: &Metadata) -> u64 {
        m.len()
    }

    pub fn device(_: &Metadata) -> Option<u64> {
        None
    }

    /// No hardlink detection: a link count of 1 means every file counts.
    pub fn identity(_: &Metadata) -> (u64, u64, u64) {
        (0, 0, 1)
    }
}

pub fn disk_usage(m: &Metadata) -> u64 {
    imp::disk_usage(m)
}

pub fn device(m: &Metadata) -> Option<u64> {
    imp::device(m)
}

/// (device, inode, link count), for hardlink detection. Linux reads these
/// with `statx` instead (see `linux.rs`).
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn identity(m: &Metadata) -> (u64, u64, u64) {
    imp::identity(m)
}
