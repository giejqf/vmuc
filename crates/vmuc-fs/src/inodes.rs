// SPDX-License-Identifier: Apache-2.0
//! Node IDs for the guest: one per host file, keyed by `(dev, ino)` so
//! hard links share a node, remembered by path relative to the share
//! root, and kept while the guest's lookup count is above zero.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::abi::ROOT_ID;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Path relative to the share root (empty for the root).
    pub rel: PathBuf,
    pub key: (u64, u64),
    pub lookups: u64,
}

#[derive(Debug)]
pub struct Inodes {
    by_id: HashMap<u64, Node>,
    by_key: HashMap<(u64, u64), u64>,
    next: u64,
}

impl Inodes {
    pub fn new(root_key: (u64, u64)) -> Self {
        let root = Node {
            rel: PathBuf::new(),
            key: root_key,
            lookups: 1,
        };
        Self {
            by_id: HashMap::from([(ROOT_ID, root)]),
            by_key: HashMap::from([(root_key, ROOT_ID)]),
            next: ROOT_ID + 1,
        }
    }

    pub fn get(&self, id: u64) -> Option<&Node> {
        self.by_id.get(&id)
    }

    pub fn contains_key(&self, key: (u64, u64)) -> bool {
        self.by_key.contains_key(&key)
    }

    /// Every node at or under `rel`, as its path and key.
    pub fn under(&self, rel: &Path) -> Vec<(PathBuf, (u64, u64))> {
        self.by_id
            .values()
            .filter(|n| n.rel.starts_with(rel))
            .map(|n| (n.rel.clone(), n.key))
            .collect()
    }

    /// The node for a file just looked up, counting one more lookup. A
    /// known file (another hard link, or the same name again) keeps its ID
    /// and takes the most recent path.
    pub fn intern(&mut self, rel: PathBuf, key: (u64, u64)) -> u64 {
        if let Some(&id) = self.by_key.get(&key)
            && let Some(n) = self.by_id.get_mut(&id)
        {
            if id != ROOT_ID {
                n.rel = rel;
            }
            n.lookups += 1;
            return id;
        }
        let id = self.next;
        self.next += 1;
        self.by_id.insert(
            id,
            Node {
                rel,
                key,
                lookups: 1,
            },
        );
        self.by_key.insert(key, id);
        id
    }

    /// FUSE_FORGET: drop `n` lookups; the node goes when none are left.
    pub fn forget(&mut self, id: u64, n: u64) {
        if id == ROOT_ID {
            return;
        }
        let gone = self.by_id.get_mut(&id).is_some_and(|node| {
            node.lookups = node.lookups.saturating_sub(n);
            node.lookups == 0
        });
        if gone && let Some(node) = self.by_id.remove(&id) {
            self.by_key.remove(&node.key);
        }
    }

    /// After a rename, every node at or under `from` lives under `to`.
    pub fn rename(&mut self, from: &Path, to: &Path) {
        for node in self.by_id.values_mut() {
            if let Ok(rest) = node.rel.strip_prefix(from) {
                node.rel = if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                };
            }
        }
    }

    /// After an exchange (`RENAME_EXCHANGE`), the two subtrees swap names.
    pub fn exchange(&mut self, a: &Path, b: &Path) {
        for node in self.by_id.values_mut() {
            if let Ok(rest) = node.rel.strip_prefix(a) {
                node.rel = b.join(rest);
            } else if let Ok(rest) = node.rel.strip_prefix(b) {
                node.rel = a.join(rest);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_count_and_forget_drops() {
        let mut t = Inodes::new((1, 1));
        let a = t.intern("a".into(), (1, 10));
        assert_eq!(t.intern("a".into(), (1, 10)), a);
        assert_eq!(t.get(a).unwrap().lookups, 2);
        t.forget(a, 1);
        assert!(t.get(a).is_some());
        t.forget(a, 1);
        assert!(t.get(a).is_none());
        t.forget(ROOT_ID, 100);
        assert!(t.get(ROOT_ID).is_some(), "the root is never forgotten");
        assert_eq!(t.intern(PathBuf::new(), (1, 1)), ROOT_ID);
    }

    #[test]
    fn hard_links_share_a_node() {
        let mut t = Inodes::new((1, 1));
        let a = t.intern("a".into(), (1, 10));
        let b = t.intern("dir/b".into(), (1, 10));
        assert_eq!(a, b);
        assert_eq!(t.get(a).unwrap().rel, Path::new("dir/b"));
    }

    #[test]
    fn renames_move_whole_subtrees_only() {
        let mut t = Inodes::new((1, 1));
        let d = t.intern("src".into(), (1, 2));
        let f = t.intern("src/lib/x.rs".into(), (1, 3));
        let other = t.intern("srcs".into(), (1, 4));
        t.rename(Path::new("src"), Path::new("dst"));
        assert_eq!(t.get(d).unwrap().rel, Path::new("dst"));
        assert_eq!(t.get(f).unwrap().rel, Path::new("dst/lib/x.rs"));
        assert_eq!(
            t.get(other).unwrap().rel,
            Path::new("srcs"),
            "a sibling with a common prefix"
        );
        t.exchange(Path::new("dst"), Path::new("srcs"));
        assert_eq!(t.get(f).unwrap().rel, Path::new("srcs/lib/x.rs"));
        assert_eq!(t.get(other).unwrap().rel, Path::new("dst"));
    }
}
