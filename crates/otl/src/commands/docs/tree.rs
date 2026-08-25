//! Rebuilding a collection's document hierarchy from a flat list.
//!
//! `documents.list` is the paginated (and therefore complete) view of a
//! collection, but it is flat: each row only points at its parent. This
//! module turns that into the forest `otl docs export` mirrors on disk.
//!
//! It is defensive on purpose, because the input is server data and the
//! consumer creates directories from it:
//!
//! - a parent that is not in the list (filtered out, or on the far side of
//!   a `--limit`) makes the document a root, never a dangling reference;
//! - a document that is its own parent, or part of a parent CYCLE, is
//!   promoted to a root, so the walk cannot loop forever;
//! - rows without an id are dropped (they could not be fetched anyway) and
//!   a duplicate id keeps its first occurrence;
//! - siblings are ordered by title then id, so re-exporting the same
//!   collection produces the same tree regardless of API ordering.
//!
//! Everything here is pure, so all of it is unit-testable without a server.

use std::collections::HashSet;

use serde_json::Value;

use crate::fields;
use crate::stdio;

/// Maximum directory nesting created by an export.
///
/// Deeper documents are written next to their parent instead of below it
/// (with a warning). The cap exists because a path grows with every level
/// and Windows still refuses long ones by default; a hierarchy this deep is
/// also unnavigable.
pub const MAX_DEPTH: usize = 8;

/// One document's place in the tree.
struct Entry<'a> {
    id: &'a str,
    title: &'a str,
    /// Position of the parent within the plan, once resolved.
    parent: Option<usize>,
}

/// A collection's documents, arranged as a forest.
pub struct Plan<'a> {
    entries: Vec<Entry<'a>>,
    children: Vec<Vec<usize>>,
    /// Top-level documents, in write order.
    pub roots: Vec<usize>,
}

impl<'a> Plan<'a> {
    /// The document id of one node.
    pub fn id(&self, node: usize) -> &'a str {
        self.entries.get(node).map(|entry| entry.id).unwrap_or("")
    }

    /// The document title of one node (may be empty).
    pub fn title(&self, node: usize) -> &'a str {
        self.entries
            .get(node)
            .map(|entry| entry.title)
            .unwrap_or("")
    }

    /// The children of one node, in write order.
    pub fn children(&self, node: usize) -> &[usize] {
        self.children.get(node).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every node below `node`, at any depth.
    ///
    /// Walked with an explicit stack and a seen-set: the forest is built
    /// from server data, so neither its depth nor (before
    /// [`break_cycles`]) its acyclicity may be assumed here.
    pub fn descendants(&self, node: usize) -> Vec<usize> {
        let mut seen = vec![false; self.entries.len()];
        let mut stack: Vec<usize> = self.children(node).to_vec();
        let mut found = Vec::new();
        while let Some(next) = stack.pop() {
            match seen.get_mut(next) {
                Some(flag) if !*flag => *flag = true,
                _ => continue,
            }
            found.push(next);
            stack.extend(self.children(next).iter().copied());
        }
        found
    }

    /// Total number of documents in the plan.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the plan holds no documents.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Build the export plan for one flat document list.
pub fn plan<'a>(documents: &'a [Value]) -> Plan<'a> {
    let (mut entries, parents) = collect(documents);
    let index = index_of(&entries);
    let mut parent: Vec<Option<usize>> = parents
        .iter()
        .enumerate()
        .map(|(position, parent)| resolve_parent(*parent, position, &index))
        .collect();
    break_cycles(&mut parent);
    for (position, resolved) in parent.iter().enumerate() {
        if let Some(entry) = entries.get_mut(position) {
            entry.parent = *resolved;
        }
    }
    let (children, roots) = link(&entries);
    Plan {
        entries,
        children,
        roots,
    }
}

/// Extract the usable rows and their raw parent ids.
///
/// A row is dropped when it has no id (it could not be fetched) and when
/// its id was already seen. The second case is not hypothetical: a server
/// whose ordering shifts between page requests can return the same document
/// on two pages, and keeping both would fetch it twice, write it twice
/// under two de-duplicated names, and inflate every count in the summary.
/// One document, one file.
fn collect<'a>(documents: &'a [Value]) -> (Vec<Entry<'a>>, Vec<Option<&'a str>>) {
    let mut entries = Vec::with_capacity(documents.len());
    let mut parents = Vec::with_capacity(documents.len());
    let mut seen: HashSet<&str> = HashSet::with_capacity(documents.len());
    let mut without_id = 0_usize;
    let mut duplicates = 0_usize;
    for document in documents {
        let Some(id) = fields::string_at(document, "/id").filter(|id| !id.is_empty()) else {
            without_id += 1;
            continue;
        };
        if !seen.insert(id) {
            duplicates += 1;
            continue;
        }
        entries.push(Entry {
            id,
            title: fields::string_at(document, "/title").unwrap_or_default(),
            parent: None,
        });
        parents.push(fields::string_at(document, "/parentDocumentId"));
    }
    if without_id > 0 {
        stdio::write_diagnostic_line(&format!(
            "warning: skipped {without_id} row(s) from the collection listing \
             that carried no document id"
        ));
    }
    if duplicates > 0 {
        stdio::write_diagnostic_line(&format!(
            "warning: the collection listing returned {duplicates} document(s) \
             more than once; each was exported a single time"
        ));
    }
    (entries, parents)
}

/// Map document id to its position.
///
/// [`collect`] has already dropped repeats, so every id appears once; the
/// `or_insert` is kept so that a future change to `collect` degrades to
/// "first occurrence wins" instead of silently re-pointing parents.
fn index_of<'a>(entries: &[Entry<'a>]) -> std::collections::HashMap<&'a str, usize> {
    let mut index = std::collections::HashMap::with_capacity(entries.len());
    for (position, entry) in entries.iter().enumerate() {
        index.entry(entry.id).or_insert(position);
    }
    index
}

/// Resolve a raw parent id to a position in the plan.
///
/// An unknown parent, or a document that claims itself as its parent, both
/// mean "root": the alternative is a dangling edge or a one-node loop.
fn resolve_parent(
    parent: Option<&str>,
    position: usize,
    index: &std::collections::HashMap<&str, usize>,
) -> Option<usize> {
    let parent = index.get(parent?).copied()?;
    (parent != position).then_some(parent)
}

/// Promote one node of every parent cycle to a root.
///
/// Without this a cyclic `parentDocumentId` chain - which a server should
/// never produce but nothing stops it from producing - would make the
/// export walk recurse forever.
fn break_cycles(parent: &mut [Option<usize>]) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum State {
        Unknown,
        Visiting,
        Done,
    }
    let mut state = vec![State::Unknown; parent.len()];
    for start in 0..parent.len() {
        if state[start] != State::Unknown {
            continue;
        }
        let mut path = Vec::new();
        let mut current = Some(start);
        while let Some(node) = current {
            match state[node] {
                State::Unknown => {
                    state[node] = State::Visiting;
                    path.push(node);
                    current = parent[node];
                }
                State::Visiting => {
                    // Closed a loop: cut it here so the node becomes a root.
                    parent[node] = None;
                    stdio::write_diagnostic_line(
                        "warning: the collection's document hierarchy contains a \
                         cycle; the documents in it were exported at the top level",
                    );
                    current = None;
                }
                State::Done => current = None,
            }
        }
        for node in path {
            state[node] = State::Done;
        }
    }
}

/// Build the children lists and the root list, both in write order.
fn link(entries: &[Entry<'_>]) -> (Vec<Vec<usize>>, Vec<usize>) {
    let mut children = vec![Vec::new(); entries.len()];
    let mut roots = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        match entry.parent {
            Some(parent) => children[parent].push(position),
            None => roots.push(position),
        }
    }
    let order = |left: &usize, right: &usize| {
        let key = |node: &usize| {
            entries
                .get(*node)
                .map(|entry| (entry.title, entry.id))
                .unwrap_or(("", ""))
        };
        key(left).cmp(&key(right))
    };
    for list in &mut children {
        list.sort_by(order);
    }
    roots.sort_by(order);
    (children, roots)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc(id: &str, title: &str, parent: Option<&str>) -> Value {
        match parent {
            Some(parent) => json!({ "id": id, "title": title, "parentDocumentId": parent }),
            None => json!({ "id": id, "title": title }),
        }
    }

    /// The plan as `(title, [child titles])` pairs, for readable asserts.
    fn shape(plan: &Plan<'_>) -> Vec<(String, Vec<String>)> {
        (0..plan.len())
            .map(|node| {
                (
                    plan.title(node).to_string(),
                    plan.children(node)
                        .iter()
                        .map(|child| plan.title(*child).to_string())
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_flat_list_becomes_all_roots() {
        let documents = vec![doc("a", "Alpha", None), doc("b", "Beta", None)];
        let plan = plan(&documents);
        assert_eq!(plan.roots, vec![0, 1]);
        assert!(plan.children(0).is_empty());
    }

    #[test]
    fn children_hang_off_their_parent() {
        let documents = vec![
            doc("a", "Alpha", None),
            doc("b", "Beta", Some("a")),
            doc("c", "Gamma", Some("b")),
        ];
        let plan = plan(&documents);
        assert_eq!(plan.roots, vec![0]);
        assert_eq!(
            shape(&plan),
            vec![
                ("Alpha".to_string(), vec!["Beta".to_string()]),
                ("Beta".to_string(), vec!["Gamma".to_string()]),
                ("Gamma".to_string(), vec![]),
            ]
        );
    }

    #[test]
    fn a_parent_outside_the_list_makes_the_document_a_root() {
        // Happens whenever a `--limit` cuts the listing, or the parent is
        // in another collection.
        let documents = vec![doc("b", "Beta", Some("missing"))];
        let plan = plan(&documents);
        assert_eq!(plan.roots, vec![0]);
    }

    #[test]
    fn a_self_parenting_document_becomes_a_root() {
        let documents = vec![doc("a", "Alpha", Some("a"))];
        let plan = plan(&documents);
        assert_eq!(plan.roots, vec![0]);
        assert!(plan.children(0).is_empty());
    }

    #[test]
    fn a_parent_cycle_is_broken_and_every_document_stays_reachable() {
        // A <- B <- A. Without breaking the loop the export would recurse
        // forever; every document must still be written exactly once.
        let documents = vec![doc("a", "Alpha", Some("b")), doc("b", "Beta", Some("a"))];
        let plan = plan(&documents);
        assert_eq!(
            reachable(&plan),
            2,
            "a document was lost: {:?}",
            shape(&plan)
        );
    }

    #[test]
    fn a_longer_cycle_is_broken_too() {
        let documents = vec![
            doc("a", "Alpha", Some("c")),
            doc("b", "Beta", Some("a")),
            doc("c", "Gamma", Some("b")),
            doc("d", "Delta", Some("a")),
        ];
        let plan = plan(&documents);
        assert_eq!(reachable(&plan), 4, "shape: {:?}", shape(&plan));
    }

    /// Count the nodes reachable from the roots, refusing to loop.
    fn reachable(plan: &Plan<'_>) -> usize {
        let mut seen = vec![false; plan.len()];
        let mut stack: Vec<usize> = plan.roots.clone();
        let mut count = 0;
        while let Some(node) = stack.pop() {
            if std::mem::replace(&mut seen[node], true) {
                continue;
            }
            count += 1;
            stack.extend(plan.children(node).iter().copied());
        }
        count
    }

    #[test]
    fn rows_without_an_id_are_dropped() {
        let documents = vec![json!({ "title": "No id" }), doc("a", "Alpha", None)];
        let plan = plan(&documents);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.title(0), "Alpha");
    }

    #[test]
    fn a_duplicate_id_is_dropped_rather_than_exported_twice() {
        // Page overlap or a shifting sort order can return one document
        // twice. Keeping both would fetch it twice and write two files.
        let documents = vec![
            doc("a", "First", None),
            doc("a", "Second", None),
            doc("b", "Child", Some("a")),
        ];
        let plan = plan(&documents);
        assert_eq!(plan.len(), 2, "the repeat was kept: {:?}", shape(&plan));
        assert_eq!(plan.title(0), "First", "the first occurrence must win");
        assert_eq!(plan.children(0), &[1]);
    }

    #[test]
    fn every_id_appears_exactly_once_in_the_plan() {
        let documents = vec![
            doc("a", "Alpha", None),
            doc("b", "Beta", Some("a")),
            doc("a", "Alpha again", None),
            doc("b", "Beta again", Some("a")),
        ];
        let plan = plan(&documents);
        let ids: Vec<&str> = (0..plan.len()).map(|node| plan.id(node)).collect();
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "duplicate ids in plan: {ids:?}");
        assert_eq!(reachable(&plan), plan.len());
    }

    #[test]
    fn siblings_are_ordered_deterministically_by_title() {
        // Re-exporting must not reshuffle the tree just because the API
        // returned rows in a different order.
        let documents = vec![
            doc("a", "Zulu", None),
            doc("b", "Alpha", None),
            doc("c", "Mike", None),
        ];
        let plan = plan(&documents);
        let titles: Vec<&str> = plan.roots.iter().map(|node| plan.title(*node)).collect();
        assert_eq!(titles, vec!["Alpha", "Mike", "Zulu"]);
    }

    #[test]
    fn identical_titles_are_ordered_by_id() {
        let documents = vec![doc("b", "Same", None), doc("a", "Same", None)];
        let plan = plan(&documents);
        let ids: Vec<&str> = plan.roots.iter().map(|node| plan.id(*node)).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn descendants_covers_the_whole_subtree() {
        let documents = vec![
            doc("a", "Alpha", None),
            doc("b", "Beta", Some("a")),
            doc("c", "Gamma", Some("b")),
            doc("d", "Delta", Some("a")),
            doc("e", "Epsilon", None),
        ];
        let plan = plan(&documents);
        let mut found: Vec<&str> = plan
            .descendants(0)
            .iter()
            .map(|node| plan.title(*node))
            .collect();
        found.sort_unstable();
        assert_eq!(found, vec!["Beta", "Delta", "Gamma"]);
        assert!(plan.descendants(4).is_empty(), "a leaf has no descendants");
    }

    #[test]
    fn descendants_of_an_unknown_node_is_empty() {
        let documents = vec![doc("a", "Alpha", None)];
        let plan = plan(&documents);
        assert!(plan.descendants(99).is_empty());
    }

    #[test]
    fn an_empty_list_is_an_empty_plan() {
        let plan = plan(&[]);
        assert!(plan.is_empty());
        assert!(plan.roots.is_empty());
    }
}
