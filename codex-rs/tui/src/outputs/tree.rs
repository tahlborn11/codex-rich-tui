//! A small, filesystem-free tree used by the Outputs file and change browsers.
//!
//! The inputs are paths already supplied by the bounded loaders.  This module
//! deliberately does not canonicalize or inspect them: rendering a tree must
//! not turn selection into another filesystem access.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntry {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeRow {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
    pub(crate) depth: usize,
    pub(crate) expanded: bool,
    pub(crate) label: String,
}

#[derive(Default)]
struct Directory {
    explicit: bool,
    directories: BTreeMap<String, Directory>,
    leaves: Vec<Leaf>,
}

struct Leaf {
    id: String,
    path: PathBuf,
    name: String,
    display_path: String,
}

/// Produces visible rows for a collapsed-by-default directory tree.
///
/// Relative inputs are rooted at `root`; absolute inputs beneath `root` are
/// represented the same way. Absolute paths outside `root` are deliberately
/// shown only by basename, so a whole-chat diff cannot disclose unrelated
/// path prefixes.
pub(crate) fn rows(
    entries: &[TreeEntry],
    root: &Path,
    expanded: &BTreeSet<PathBuf>,
    query: &str,
) -> Vec<TreeRow> {
    let mut tree = Directory::default();
    let mut outside = Vec::new();

    for entry in entries {
        let Some(relative) = relative_path(&entry.path, root) else {
            if !entry.is_dir {
                outside.push(Leaf {
                    id: entry.id.clone(),
                    path: entry.path.clone(),
                    name: basename(&entry.path),
                    display_path: basename(&entry.path),
                });
            }
            continue;
        };
        insert(&mut tree, relative, entry, root);
    }

    let query = query.trim().to_lowercase();
    let mut output = Vec::new();
    flatten_directory(&tree, root, Path::new(""), 0, expanded, &query, &mut output);

    outside.sort_by(|a, b| name_order(&a.name, &b.name));
    for leaf in outside {
        if query.is_empty() || leaf.display_path.to_lowercase().contains(&query) {
            output.push(TreeRow {
                id: leaf.id,
                path: leaf.path,
                is_dir: false,
                depth: 0,
                expanded: false,
                label: format!("  {}", leaf.name),
            });
        }
    }
    output
}

fn relative_path<'a>(path: &'a Path, root: &Path) -> Option<&'a Path> {
    if path.is_absolute() {
        path.strip_prefix(root).ok()
    } else {
        Some(path)
    }
}

fn insert(tree: &mut Directory, relative: &Path, entry: &TreeEntry, root: &Path) {
    let components: Vec<String> = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if components.is_empty() {
        tree.explicit |= entry.is_dir;
        return;
    }

    let (parents, name) = components.split_at(components.len() - 1);
    let mut directory = tree;
    for parent in parents {
        directory = directory.directories.entry(parent.clone()).or_default();
    }
    let name = &name[0];
    let normalized_path = root.join(relative);
    if entry.is_dir {
        directory
            .directories
            .entry(name.clone())
            .or_default()
            .explicit = true;
    } else {
        directory.leaves.push(Leaf {
            id: entry.id.clone(),
            path: normalized_path,
            name: name.clone(),
            display_path: relative.to_string_lossy().into_owned(),
        });
    }
}

fn flatten_directory(
    directory: &Directory,
    root: &Path,
    relative: &Path,
    depth: usize,
    expanded: &BTreeSet<PathBuf>,
    query: &str,
    output: &mut Vec<TreeRow>,
) -> bool {
    let self_matches = !relative.as_os_str().is_empty()
        && relative.to_string_lossy().to_lowercase().contains(query);
    let mut children_match = false;
    for (name, child) in sorted_directories(directory) {
        if flatten_matches(child, &relative.join(name), query) {
            children_match = true;
            break;
        }
    }
    let leaves_match = directory
        .leaves
        .iter()
        .any(|leaf| leaf.display_path.to_lowercase().contains(query));
    let visible = query.is_empty() || self_matches || children_match || leaves_match;
    if !visible {
        return false;
    }

    let root_node = relative.as_os_str().is_empty();
    let path = root.join(relative);
    // A search opens all visible directory branches so every matching item is
    // reachable. Root is implicit and therefore needs no expansion row.
    let effective_expanded = !query.is_empty() || expanded.contains(&path);
    if !root_node {
        let name = basename(relative);
        output.push(TreeRow {
            id: format!("directory:{}", path.display()),
            path,
            is_dir: true,
            depth,
            expanded: effective_expanded,
            label: format!(
                "{}{} {}/",
                "  ".repeat(depth),
                if effective_expanded { '▾' } else { '▸' },
                name
            ),
        });
    }

    // The implicit root is always open. All other directories obey their
    // expansion state unless a filter is active.
    if root_node || effective_expanded {
        let child_depth = if root_node { 0 } else { depth + 1 };
        for (name, child) in sorted_directories(directory) {
            flatten_directory(
                child,
                root,
                &relative.join(name),
                child_depth,
                expanded,
                query,
                output,
            );
        }
        let mut leaves: Vec<&Leaf> = directory.leaves.iter().collect();
        leaves.sort_by(|a, b| name_order(&a.name, &b.name));
        for leaf in leaves {
            if query.is_empty() || self_matches || leaf.display_path.to_lowercase().contains(query)
            {
                output.push(TreeRow {
                    id: leaf.id.clone(),
                    path: leaf.path.clone(),
                    is_dir: false,
                    depth: child_depth,
                    expanded: false,
                    label: format!("{}  {}", "  ".repeat(child_depth), leaf.name),
                });
            }
        }
    }
    true
}

fn flatten_matches(directory: &Directory, relative: &Path, query: &str) -> bool {
    if relative.to_string_lossy().to_lowercase().contains(query) {
        return true;
    }
    directory
        .leaves
        .iter()
        .any(|leaf| leaf.display_path.to_lowercase().contains(query))
        || directory
            .directories
            .iter()
            .any(|(name, child)| flatten_matches(child, &relative.join(name), query))
}

fn sorted_directories(directory: &Directory) -> Vec<(&String, &Directory)> {
    let mut directories: Vec<_> = directory.directories.iter().collect();
    directories.sort_by(|(left, _), (right, _)| name_order(left, right));
    directories
}

fn basename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn name_order(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, path: &str, is_dir: bool) -> TreeEntry {
        TreeEntry {
            id: id.into(),
            path: path.into(),
            is_dir,
        }
    }

    fn labels(rows: &[TreeRow]) -> Vec<&str> {
        rows.iter().map(|row| row.label.as_str()).collect()
    }

    #[test]
    fn keeps_an_explicit_empty_directory() {
        let root = Path::new("/work");
        let result = rows(&[entry("empty", "empty", true)], root, &BTreeSet::new(), "");
        assert_eq!(labels(&result), vec!["▸ empty/"]);
        assert_eq!(result[0].path, PathBuf::from("/work/empty"));
    }

    #[test]
    fn nested_directories_expand_only_when_selected() {
        let root = Path::new("/work");
        let inputs = [entry("readme", "src/lib/readme.md", false)];
        assert_eq!(
            labels(&rows(&inputs, root, &BTreeSet::new(), "")),
            vec!["▸ src/"]
        );
        let expanded = BTreeSet::from([PathBuf::from("/work/src"), PathBuf::from("/work/src/lib")]);
        assert_eq!(
            labels(&rows(&inputs, root, &expanded, "")),
            vec!["▾ src/", "  ▾ lib/", "      readme.md"]
        );
    }

    #[test]
    fn sorts_directories_before_files_and_by_name() {
        let root = Path::new("/work");
        let inputs = [
            entry("z", "z.txt", false),
            entry("a", "Alpha.txt", false),
            entry("dir-z", "Zed", true),
            entry("dir-a", "apple", true),
        ];
        assert_eq!(
            labels(&rows(&inputs, root, &BTreeSet::new(), "")),
            vec!["▸ apple/", "▸ Zed/", "  Alpha.txt", "  z.txt"]
        );
    }

    #[test]
    fn filter_keeps_ancestors_and_opens_matching_directory_descendants() {
        let root = Path::new("/work");
        let inputs = [
            entry("target", "src/components/button.rs", false),
            entry("other", "src/components/input.rs", false),
            entry("doc", "docs/guide.md", false),
        ];
        assert_eq!(
            labels(&rows(&inputs, root, &BTreeSet::new(), "button")),
            vec!["▾ src/", "  ▾ components/", "      button.rs"]
        );
        assert_eq!(
            labels(&rows(&inputs, root, &BTreeSet::new(), "components")),
            vec![
                "▾ src/",
                "  ▾ components/",
                "      button.rs",
                "      input.rs"
            ]
        );
    }

    #[test]
    fn normalizes_absolute_and_relative_paths_beneath_root() {
        let root = Path::new("/work");
        let inputs = [
            entry("relative", "src/lib.rs", false),
            entry("absolute", "/work/src/main.rs", false),
        ];
        let expanded = BTreeSet::from([PathBuf::from("/work/src")]);
        let result = rows(&inputs, root, &expanded, "");
        assert_eq!(labels(&result), vec!["▾ src/", "    lib.rs", "    main.rs"]);
        assert_eq!(result[1].path, PathBuf::from("/work/src/lib.rs"));
        assert_eq!(result[2].path, PathBuf::from("/work/src/main.rs"));
    }

    #[test]
    fn outside_paths_are_basename_only_without_synthetic_parents() {
        let root = Path::new("/work");
        let result = rows(
            &[entry("external", "/private/tmp/plan.md", false)],
            root,
            &BTreeSet::new(),
            "",
        );
        assert_eq!(labels(&result), vec!["  plan.md"]);
        assert_eq!(result[0].path, PathBuf::from("/private/tmp/plan.md"));
    }

    #[test]
    fn directory_and_file_with_same_name_do_not_collide() {
        let root = Path::new("/work");
        let inputs = [
            entry("directory", "same", true),
            entry("file", "same", false),
        ];
        let result = rows(&inputs, root, &BTreeSet::new(), "");
        assert_eq!(labels(&result), vec!["▸ same/", "  same"]);
        assert_eq!(result[0].id, "directory:/work/same");
        assert_eq!(result[1].id, "file");
    }

    #[test]
    fn preserves_unicode_names() {
        let root = Path::new("/work");
        let inputs = [entry("heart", "資料/💙.md", false)];
        let expanded = BTreeSet::from([PathBuf::from("/work/資料")]);
        assert_eq!(
            labels(&rows(&inputs, root, &expanded, "💙")),
            vec!["▾ 資料/", "    💙.md"]
        );
    }
}
