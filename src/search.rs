//! Finds entries by name below a directory.

use rayon::prelude::*;

use crate::tree::{NodeId, Tree};

/// A case-insensitive name matcher: a substring, or a glob when the query
/// contains `*` or `?` (matched against the whole name).
pub enum Matcher {
    Substring(Vec<u8>),
    Glob(Vec<u8>),
}

impl Matcher {
    pub fn new(query: &str) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let q = query.as_bytes().to_ascii_lowercase();
        Some(if q.contains(&b'*') || q.contains(&b'?') { Matcher::Glob(q) } else { Matcher::Substring(q) })
    }

    pub fn matches(&self, name: &[u8]) -> bool {
        match self {
            Matcher::Substring(q) => name.len() >= q.len() && name.windows(q.len()).any(|w| w.eq_ignore_ascii_case(q)),
            Matcher::Glob(p) => glob(p, name),
        }
    }
}

/// Iterative wildcard match with single-star backtracking.
fn glob(pat: &[u8], name: &[u8]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == name[n].to_ascii_lowercase()) {
            p += 1;
            n += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((sp, sn)) = star {
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

pub struct Results {
    /// Matching entries, largest first.
    pub ids: Vec<NodeId>,
    /// Total size of the matches. Never double counted: the contents of a
    /// matching directory are not searched.
    pub total: u64,
}

/// Below this many entries a plain walk of the subtree is fastest.
const PARALLEL_THRESHOLD: u32 = 50_000;

/// Finds the top-most entries below `under` whose names match. Runs on the
/// current rayon pool for large subtrees.
pub fn find(tree: &Tree, under: NodeId, m: &Matcher) -> Results {
    let mut ids = if tree.get(under).items < PARALLEL_THRESHOLD { walk(tree, under, m) } else { scan_all(tree, under, m) };
    ids.sort_unstable_by(|&a, &b| tree.get(b).disk.cmp(&tree.get(a).disk));
    let total = ids.iter().map(|&id| tree.get(id).disk).sum();
    Results { ids, total }
}

/// Depth-first walk that doesn't descend into matches.
fn walk(tree: &Tree, under: NodeId, m: &Matcher) -> Vec<NodeId> {
    let mut ids = Vec::new();
    let mut stack: Vec<NodeId> = tree.children(under).to_vec();
    while let Some(id) = stack.pop() {
        if !tree.is_live(id) {
            continue;
        }
        if m.matches(tree.name(id).as_encoded_bytes()) {
            ids.push(id);
        } else {
            stack.extend_from_slice(tree.children(id));
        }
    }
    ids
}

/// Tests every node in the arena in parallel; only matches pay for the walk
/// up to check that they are below `under` with no matching ancestor.
fn scan_all(tree: &Tree, under: NodeId, m: &Matcher) -> Vec<NodeId> {
    let is_match = |id: NodeId| m.matches(tree.name(id).as_encoded_bytes());
    (0..tree.len() as NodeId)
        .into_par_iter()
        .filter(|&id| id != under && tree.is_live(id) && is_match(id))
        .filter(|&id| {
            let mut cur = id;
            while let Some(p) = tree.parent(cur) {
                if p == under {
                    return true;
                }
                if is_match(p) {
                    return false;
                }
                cur = p;
            }
            false
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str, name: &str) -> bool {
        Matcher::new(q).unwrap().matches(name.as_bytes())
    }

    #[test]
    fn walk_and_parallel_scan_agree() {
        use crate::scan::{Entry, Msg};
        use crate::tree::{Kind, ROOT};
        let e = |name: &str, kind| Entry { name: name.into(), kind, disk: 10, flags: 0 };
        // /r: target/ (holds target/ and x.o), src/ (holds main.rs, target.txt), target.log
        let mut t = Tree::new("/r".into());
        t.apply(Msg::Batch { dir: ROOT, first: 1, entries: vec![e("target", Kind::Dir), e("src", Kind::Dir), e("target.log", Kind::File)], error: false });
        t.apply(Msg::Batch { dir: 1, first: 4, entries: vec![e("target", Kind::Dir), e("x.o", Kind::File)], error: false });
        t.apply(Msg::Batch { dir: 2, first: 6, entries: vec![e("main.rs", Kind::File), e("target.txt", Kind::File)], error: false });
        t.apply(Msg::Batch { dir: 4, first: 8, entries: vec![], error: false });

        let m = Matcher::new("target").unwrap();
        for under in [ROOT, 2] {
            let mut a = walk(&t, under, &m);
            let mut b = scan_all(&t, under, &m);
            a.sort();
            b.sort();
            assert_eq!(a, b, "under {under}");
        }
        // The nested target/target is inside a match, so it's not listed.
        let mut top = walk(&t, ROOT, &m);
        top.sort();
        assert_eq!(top, vec![1, 3, 7]);
        assert_eq!(walk(&t, 2, &m), vec![7]);
    }

    #[test]
    fn substring() {
        assert!(m("mod", "node_modules"));
        assert!(m("MOD", "node_Modules"));
        assert!(!m("modz", "node_modules"));
        assert!(Matcher::new("").is_none());
    }

    #[test]
    fn globs() {
        assert!(m("*.rs", "main.rs"));
        assert!(!m("*.rs", "main.rsx"));
        assert!(m("ca?go*", "Cargo.toml"));
        assert!(m("*a*b*c", "xxaYYbZZc"));
        assert!(!m("*a*b*c", "xxaYYcZZb"));
        assert!(m("*", ""));
        assert!(!m("?", ""));
    }
}
