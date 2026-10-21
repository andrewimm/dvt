//! Shell-style wildcard matching on raw bytes: `*` matches any run of bytes
//! (including `/`), `?` matches exactly one.

/// Whether `text` matches the whole of `pat`. With `fold_case`, ASCII letters
/// in `text` are lowercased before comparing, so `pat` must already be
/// lowercase.
pub fn matches(pat: &[u8], text: &[u8], fold_case: bool) -> bool {
    let fold = |b: u8| if fold_case { b.to_ascii_lowercase() } else { b };
    let (mut p, mut t) = (0, 0);
    // Where the last `*` was, and how much of `text` it has swallowed so far.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == fold(text[t])) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn wildcards() {
        assert!(matches(b"*.rs", b"main.rs", false));
        assert!(!matches(b"*.rs", b"main.rsx", false));
        assert!(matches(b"ca?go*", b"cargo.toml", false));
        assert!(matches(b"*a*b*c", b"xxaYYbZZc", false));
        assert!(!matches(b"*a*b*c", b"xxaYYcZZb", false));
        assert!(matches(b"*", b"", false));
        assert!(!matches(b"?", b"", false));
        assert!(matches(b"/home/*/.cache", b"/home/andrew/.cache", false));
    }

    #[test]
    fn case() {
        assert!(!matches(b"target", b"Target", false));
        assert!(matches(b"target", b"Target", true));
    }
}
