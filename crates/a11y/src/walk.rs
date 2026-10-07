//! Bounded, pipelined breadth-first walk (design D17). Generic over the node
//! reader so it is tested with synthetic trees.

use std::future::Future;
use std::time::Instant;

use futures::StreamExt;
use futures::stream;

use crate::rule::{is_interactive, is_web_content};

/// What the walk needs to know about one node.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeData<N> {
    pub role: String,
    /// SHOWING and VISIBLE.
    pub showing: bool,
    pub children: Vec<N>,
}

#[derive(Debug, Clone, Copy)]
pub struct WalkLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub concurrency: usize,
    pub deadline: Instant,
    pub skip_web_content: bool,
}

pub const DEFAULT_MAX_NODES: usize = 1500;
pub const DEFAULT_MAX_DEPTH: usize = 40;
pub const CONCURRENCY: usize = 16;

/// An interactive node found by the walk, with its parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Found<N> {
    pub node: N,
    pub parent: Option<N>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WalkResult<N> {
    pub found: Vec<Found<N>>,
    pub visited: usize,
    pub truncated: bool,
}

/// Walk from `root` level by level, reading up to `concurrency` nodes at a
/// time with `read`. Nodes that fail to read are skipped. Hidden nodes are
/// not descended into (their children cannot be on screen).
pub async fn walk<N, F, Fut>(root: N, limits: WalkLimits, read: F) -> WalkResult<N>
where
    N: Clone,
    F: Fn(N) -> Fut,
    Fut: Future<Output = Option<NodeData<N>>>,
{
    let mut found = Vec::new();
    let mut visited = 0usize;
    let mut truncated = false;
    let mut level: Vec<(N, Option<N>)> = vec![(root, None)];
    let mut depth = 0usize;
    while !level.is_empty() {
        if depth > limits.max_depth {
            truncated = true;
            break;
        }
        let room = limits.max_nodes.saturating_sub(visited);
        if level.len() > room {
            level.truncate(room);
            truncated = true;
        }
        if level.is_empty() {
            break;
        }
        let read = &read;
        let mut results = stream::iter(level.into_iter().map(|(n, parent)| async move {
            let d = read(n.clone()).await;
            (n, parent, d)
        }))
        .buffer_unordered(limits.concurrency.max(1));
        let mut next = Vec::new();
        while let Some((n, parent, data)) = results.next().await {
            visited += 1;
            let Some(d) = data else { continue };
            // The root frame itself is never a hint.
            if depth > 0 && d.showing && is_interactive(&d.role) {
                found.push(Found {
                    node: n.clone(),
                    parent: parent.clone(),
                    role: d.role.clone(),
                });
            }
            if (depth > 0 && !d.showing) || (limits.skip_web_content && is_web_content(&d.role)) {
                continue;
            }
            next.extend(d.children.into_iter().map(|c| (c, Some(n.clone()))));
        }
        if Instant::now() >= limits.deadline && !next.is_empty() {
            truncated = true;
            break;
        }
        level = next;
        depth += 1;
    }
    WalkResult {
        found,
        visited,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    type Tree = HashMap<u32, (&'static str, bool, Vec<u32>)>;

    fn limits() -> WalkLimits {
        WalkLimits {
            max_nodes: DEFAULT_MAX_NODES,
            max_depth: DEFAULT_MAX_DEPTH,
            concurrency: CONCURRENCY,
            deadline: Instant::now() + Duration::from_secs(5),
            skip_web_content: false,
        }
    }

    async fn run(t: &Tree, l: WalkLimits) -> WalkResult<u32> {
        walk(0u32, l, |n| async move {
            t.get(&n).map(|(r, s, c)| NodeData {
                role: (*r).to_owned(),
                showing: *s,
                children: c.clone(),
            })
        })
        .await
    }

    fn tree() -> Tree {
        HashMap::from([
            (0, ("frame", true, vec![1, 2, 3])),
            (1, ("push button", true, vec![])),
            (2, ("panel", true, vec![4, 5, 9])),
            (3, ("document web", true, vec![6])),
            (4, ("entry", true, vec![])),
            (5, ("panel", false, vec![7])),
            (6, ("link", true, vec![])),
            (7, ("push button", true, vec![])),
            (9, ("label", true, vec![])),
        ])
    }

    #[tokio::test]
    async fn keeps_showing_interactive() {
        let r = run(&tree(), limits()).await;
        let mut ids: Vec<u32> = r.found.iter().map(|f| f.node).collect();
        ids.sort();
        assert_eq!(ids, [1, 4, 6]);
        assert!(!r.truncated);
        assert_eq!(r.visited, 8);
        let entry = r.found.iter().find(|f| f.node == 4).unwrap();
        assert_eq!(entry.parent, Some(2));
        assert_eq!(entry.role, "entry");
    }

    #[tokio::test]
    async fn skips_web_content() {
        let mut l = limits();
        l.skip_web_content = true;
        let r = run(&tree(), l).await;
        assert!(r.found.iter().all(|f| f.node != 6));
        assert_eq!(r.visited, 7);
    }

    #[tokio::test]
    async fn node_budget_truncates() {
        let mut l = limits();
        l.max_nodes = 3;
        let r = run(&tree(), l).await;
        assert!(r.truncated);
        assert_eq!(r.visited, 3);
    }

    #[tokio::test]
    async fn depth_cap_truncates() {
        let mut l = limits();
        l.max_depth = 1;
        let r = run(&tree(), l).await;
        assert!(r.truncated);
        assert!(r.found.iter().all(|f| f.node == 1));
    }

    #[tokio::test]
    async fn deadline_truncates() {
        let mut l = limits();
        l.deadline = Instant::now();
        let r = run(&tree(), l).await;
        assert!(r.truncated);
        assert_eq!(r.visited, 1);
    }

    #[tokio::test]
    async fn unreadable_nodes_skipped() {
        let mut t = tree();
        t.remove(&2);
        let r = run(&t, limits()).await;
        let ids: Vec<u32> = r.found.iter().map(|f| f.node).collect();
        assert!(!ids.contains(&4));
        assert!(!r.truncated);
    }
}
