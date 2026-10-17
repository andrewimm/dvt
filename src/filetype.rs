//! Classifies entries by file extension, for colouring.

use std::ffi::OsStr;

use crate::tree::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Dir,
    Media,
    Archive,
    Code,
    Document,
    Build,
    Data,
    Other,
}

impl Category {
    pub const ALL: [Category; 8] = [
        Category::Dir,
        Category::Media,
        Category::Archive,
        Category::Code,
        Category::Document,
        Category::Build,
        Category::Data,
        Category::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Dir => "dir",
            Category::Media => "media",
            Category::Archive => "archive",
            Category::Code => "code/text",
            Category::Document => "doc",
            Category::Build => "build",
            Category::Data => "data",
            Category::Other => "other",
        }
    }
}

const MEDIA: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif", "svg", "ico", "raw", "cr2", "cr3",
    "nef", "arw", "dng", "psd", "xcf", "kra", "exr", "hdr", "blend", "mp4", "mkv", "mov", "avi", "webm", "m4v", "wmv",
    "flv", "mpg", "mpeg", "3gp", "mp3", "flac", "wav", "ogg", "opus", "m4a", "aac", "wma", "aiff", "mid", "midi",
];
const ARCHIVE: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "txz", "zst", "zstd", "lz4", "lzma", "7z", "rar", "iso", "img", "dmg",
    "deb", "rpm", "apk", "pkg", "snap", "flatpak", "appimage", "qcow2", "vdi", "vmdk", "vhd", "vhdx", "cab", "msi",
    "jar", "war", "whl", "crate", "gem", "nupkg",
];
const CODE: &[&str] = &[
    "rs", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "py", "pyi", "js", "mjs", "cjs", "ts", "tsx", "jsx", "go", "java",
    "kt", "kts", "scala", "swift", "m", "mm", "cs", "fs", "rb", "php", "pl", "lua", "sh", "bash", "zsh", "fish", "ps1",
    "bat", "sql", "r", "jl", "ex", "exs", "erl", "hs", "ml", "clj", "dart", "zig", "nim", "asm", "s", "html", "htm",
    "css", "scss", "sass", "less", "vue", "svelte", "md", "rst", "txt", "toml", "yaml", "yml", "json", "jsonc", "xml",
    "ini", "cfg", "conf", "lock", "gradle", "cmake", "mk", "nix", "tex", "patch", "diff", "glsl", "hlsl", "wgsl",
];
const CODE_NAMES: &[&str] = &["makefile", "dockerfile", "license", "readme", "justfile", "rakefile", "gemfile"];
const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp", "epub", "mobi", "djvu", "pages",
    "numbers", "key",
];
const BUILD: &[&str] = &[
    "o", "obj", "a", "so", "dll", "dylib", "lib", "exe", "bin", "elf", "rlib", "rmeta", "d", "class", "pyc", "pyo",
    "wasm", "dex", "out", "pdb", "ilk", "pch", "gch", "node",
];
const DATA: &[&str] = &[
    "db", "sqlite", "sqlite3", "mdb", "parquet", "arrow", "feather", "avro", "orc", "h5", "hdf5", "npy", "npz", "pt",
    "pth", "ckpt", "safetensors", "gguf", "ggml", "onnx", "tflite", "pb", "pkl", "pickle", "csv", "tsv", "log", "bak",
    "dump", "pack", "idx", "sav", "mbox", "pst", "ost",
];

pub fn categorize(kind: Kind, name: &OsStr) -> Category {
    if kind == Kind::Dir {
        return Category::Dir;
    }
    let bytes = name.as_encoded_bytes();
    // A leading dot marks a hidden file, not an extension.
    let ext = match bytes.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 => &bytes[i + 1..],
        _ => {
            return if CODE_NAMES.iter().any(|n| n.as_bytes().eq_ignore_ascii_case(bytes)) {
                Category::Code
            } else {
                Category::Other
            };
        }
    };
    let is = |list: &[&str]| list.iter().any(|e| e.as_bytes().eq_ignore_ascii_case(ext));
    if is(MEDIA) {
        Category::Media
    } else if is(ARCHIVE) {
        Category::Archive
    } else if is(CODE) {
        Category::Code
    } else if is(DOCUMENT) {
        Category::Document
    } else if is(BUILD) {
        Category::Build
    } else if is(DATA) {
        Category::Data
    } else {
        Category::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(name: &str) -> Category {
        categorize(Kind::File, OsStr::new(name))
    }

    #[test]
    fn categories() {
        assert_eq!(categorize(Kind::Dir, OsStr::new("x.zip")), Category::Dir);
        assert_eq!(cat("IMG_0001.JPG"), Category::Media);
        assert_eq!(cat("backup.tar.gz"), Category::Archive);
        assert_eq!(cat("main.rs"), Category::Code);
        assert_eq!(cat("Makefile"), Category::Code);
        assert_eq!(cat("paper.pdf"), Category::Document);
        assert_eq!(cat("libfoo.rlib"), Category::Build);
        assert_eq!(cat("model.safetensors"), Category::Data);
        assert_eq!(cat(".bashrc"), Category::Other);
        assert_eq!(cat("noext"), Category::Other);
        assert_eq!(cat("trailingdot."), Category::Other);
    }
}
