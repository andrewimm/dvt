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

    pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
        (m.nlink() > 1).then(|| (m.dev(), m.ino()))
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

    pub fn hardlink_key(_: &Metadata) -> Option<(u64, u64)> {
        None
    }
}

pub fn disk_usage(m: &Metadata) -> u64 {
    imp::disk_usage(m)
}

pub fn device(m: &Metadata) -> Option<u64> {
    imp::device(m)
}

pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
    imp::hardlink_key(m)
}
