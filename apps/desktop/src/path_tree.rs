//! Turn a flat list of file paths into the rows of a collapsible tree.
//!
//! Pure and GPUI-free: the shape of a tree is fiddly enough (compacted chains,
//! collapsed subtrees, ordering) to be worth testing on its own, separately from
//! how it is painted.
//!
//! Two conventions borrowed from the tools this mirrors:
//!
//! * **Directories before files**, each alphabetical — the JetBrains change
//!   browser and GitHub's file tree both do this, and it keeps a file's position
//!   stable as siblings appear.
//! * **Compacted middle directories** — a chain like `crates/domain/src` with
//!   nothing else in it renders as one row rather than three, so a deep tree of
//!   single-child directories doesn't push filenames off the panel. JetBrains
//!   calls this "compact middle packages".

use std::collections::BTreeMap;

/// One rendered line of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Indent level, 0 at the root.
    pub depth: usize,
    /// What to print — a (possibly compacted) directory name, or a file name.
    pub label: String,
    pub kind: TreeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeKind {
    /// A directory, identified by its full prefix so callers can toggle it.
    Dir { path: String, collapsed: bool },
    /// A file, carrying the caller's index into the list it supplied.
    File { index: usize },
}

/// Build the visible rows for `paths` (each an index and a display path, `/`
/// separated), hiding anything under a directory in `collapsed`.
///
/// Paths are taken as given — sort them beforehand if the caller wants a stable
/// order; the grouping below is deterministic either way.
pub fn rows(paths: &[(usize, String)], collapsed: &dyn Fn(&str) -> bool) -> Vec<TreeRow> {
    let mut out = Vec::new();
    build(paths, "", 0, collapsed, &mut out);
    out
}

/// Emit the rows for one directory level: its subdirectories (recursing), then its
/// own files.
fn build(
    paths: &[(usize, String)],
    prefix: &str,
    depth: usize,
    collapsed: &dyn Fn(&str) -> bool,
    out: &mut Vec<TreeRow>,
) {
    // Group this level's entries by their first path segment. `BTreeMap` gives the
    // alphabetical order for free and keeps the walk deterministic.
    let mut dirs: BTreeMap<&str, Vec<(usize, String)>> = BTreeMap::new();
    let mut files: Vec<(usize, &str)> = Vec::new();
    for (index, path) in paths {
        match path.split_once('/') {
            Some((head, rest)) => dirs
                .entry(head)
                .or_default()
                .push((*index, rest.to_string())),
            None => files.push((*index, path.as_str())),
        }
    }

    for (name, children) in dirs {
        // Compact a single-child chain: `a` holding only `b/…` renders as `a/b`.
        let mut label = name.to_string();
        let mut path = join(prefix, name);
        let mut children = children;
        while !collapsed(&path) {
            let only_dir = single_child_dir(&children);
            let Some(next) = only_dir else { break };
            label = format!("{label}/{next}");
            path = join(&path, &next);
            children = children
                .into_iter()
                .filter_map(|(i, p)| p.split_once('/').map(|(_, rest)| (i, rest.to_string())))
                .collect();
        }

        let is_collapsed = collapsed(&path);
        out.push(TreeRow {
            depth,
            label,
            kind: TreeKind::Dir {
                path: path.clone(),
                collapsed: is_collapsed,
            },
        });
        if !is_collapsed {
            build(&children, &path, depth + 1, collapsed, out);
        }
    }

    files.sort_by(|a, b| a.1.cmp(b.1));
    for (index, name) in files {
        out.push(TreeRow {
            depth,
            label: name.to_string(),
            kind: TreeKind::File { index },
        });
    }
}

/// The single subdirectory every entry sits under, when there is exactly one and
/// no files alongside it — the condition for compacting a chain.
fn single_child_dir(children: &[(usize, String)]) -> Option<String> {
    let mut only: Option<&str> = None;
    for (_, path) in children {
        let (head, _) = path.split_once('/')?; // a file here ⇒ no compaction
        match only {
            Some(seen) if seen != head => return None,
            _ => only = Some(head),
        }
    }
    only.map(str::to_string)
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(paths: &[&str]) -> Vec<(usize, String)> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i, (*p).to_string()))
            .collect()
    }

    /// `depth:label` per row, so a whole tree shape reads in one assertion.
    fn shape(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| {
                let mark = match r.kind {
                    TreeKind::Dir { .. } => "/",
                    TreeKind::File { .. } => "",
                };
                format!("{}:{}{}", r.depth, r.label, mark)
            })
            .collect()
    }

    fn nothing_collapsed(_: &str) -> bool {
        false
    }

    #[test]
    fn files_group_under_their_directories() {
        let paths = input(&["src/main.rs", "src/lib.rs", "README.md"]);
        assert_eq!(
            shape(&rows(&paths, &nothing_collapsed)),
            // Directory first, then this level's files; each alphabetical.
            vec!["0:src/", "1:lib.rs", "1:main.rs", "0:README.md"]
        );
    }

    #[test]
    fn a_single_child_chain_is_compacted_into_one_row() {
        let paths = input(&["crates/domain/src/changes.rs"]);
        assert_eq!(
            shape(&rows(&paths, &nothing_collapsed)),
            vec!["0:crates/domain/src/", "1:changes.rs"],
            "three nested directories with one child each render as one row"
        );
    }

    #[test]
    fn a_chain_stops_compacting_where_it_branches() {
        let paths = input(&["a/b/c/one.rs", "a/b/d/two.rs"]);
        assert_eq!(
            shape(&rows(&paths, &nothing_collapsed)),
            vec!["0:a/b/", "1:c/", "2:one.rs", "1:d/", "2:two.rs"]
        );
    }

    #[test]
    fn a_directory_holding_a_file_does_not_absorb_its_subdirectory() {
        // `a` has both a file and a subdirectory, so compacting it would hide the file.
        let paths = input(&["a/keep.rs", "a/b/deep.rs"]);
        assert_eq!(
            shape(&rows(&paths, &nothing_collapsed)),
            vec!["0:a/", "1:b/", "2:deep.rs", "1:keep.rs"]
        );
    }

    #[test]
    fn collapsing_a_directory_hides_everything_under_it() {
        let paths = input(&["src/a.rs", "src/b.rs", "top.rs"]);
        let rows = rows(&paths, &|path| path == "src");
        assert_eq!(shape(&rows), vec!["0:src/", "0:top.rs"]);
        assert!(
            matches!(
                rows[0].kind,
                TreeKind::Dir {
                    collapsed: true,
                    ..
                }
            ),
            "and the row reports itself collapsed so the caret can point right"
        );
    }

    #[test]
    fn a_collapsed_directory_is_not_compacted_through() {
        // Collapsing `crates` must not silently expand it into `crates/domain/src`.
        let paths = input(&["crates/domain/src/changes.rs"]);
        let rows = rows(&paths, &|path| path == "crates");
        assert_eq!(shape(&rows), vec!["0:crates/"]);
    }

    #[test]
    fn file_indices_survive_the_grouping() {
        // The caller looks its own data up by index, so the mapping must be exact.
        let paths = input(&["z/last.rs", "a/first.rs"]);
        let rows = rows(&paths, &nothing_collapsed);
        let files: Vec<_> = rows
            .iter()
            .filter_map(|r| match r.kind {
                TreeKind::File { index } => Some((index, r.label.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(files, vec![(1, "first.rs"), (0, "last.rs")]);
    }

    #[test]
    fn an_empty_list_has_no_rows() {
        assert!(rows(&[], &nothing_collapsed).is_empty());
    }
}
