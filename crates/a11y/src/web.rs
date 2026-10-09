//! WebKitGTK web-root discovery. The page lives in a separate web process
//! with its own bus connection; the UI process's Collection does not cross
//! into it and the WebKitWebView above it is not SHOWING. Generic over the
//! node reader so it is tested with synthetic trees.

use std::future::Future;
use std::time::Instant;

use futures::StreamExt;
use futures::stream;

/// Levels below a crossing searched for a Collection node.
pub const CROSS_DEPTH: usize = 4;

/// A node that knows the bus it lives on.
pub trait OnBus {
    fn bus(&self) -> &str;
}

impl OnBus for crate::dbus::Obj {
    fn bus(&self) -> &str {
        &self.bus
    }
}

/// What discovery needs to know about one node.
#[derive(Debug, Clone, PartialEq)]
pub struct WebNode<N> {
    /// SHOWING and VISIBLE; only read on the application's bus.
    pub showing: bool,
    /// Implements `org.a11y.atspi.Collection`; only read across the bus.
    pub has_collection: bool,
    pub children: Vec<N>,
}

/// One web view: the UI-side node (`view`), its child on another bus
/// (`crossing`) and the first Collection node below it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct WebRoot<N> {
    pub root: Option<N>,
    pub crossing: N,
    pub view: N,
}

#[derive(Debug, Clone, Copy)]
pub struct DiscoverLimits {
    pub max_nodes: usize,
    pub concurrency: usize,
    pub deadline: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Discovery<N> {
    pub roots: Vec<WebRoot<N>>,
    pub visited: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy)]
enum At {
    /// On the application's bus, `depth` below the frame.
    Same { depth: usize },
    /// `level` below crossing `idx`.
    Cross { idx: usize, level: usize },
}

/// Breadth-first from `frame`. Same-bus nodes below the frame that are not
/// SHOWING are descended only into children on another bus; across the bus,
/// nodes are descended regardless of state for [`CROSS_DEPTH`] levels and the
/// first Collection node becomes that crossing's root. `read(node, crossed)`
/// is told whether the node is across the bus.
pub async fn discover<N, F, Fut>(
    frame: N,
    app_bus: &str,
    limits: DiscoverLimits,
    read: F,
) -> Discovery<N>
where
    N: Clone + OnBus,
    F: Fn(N, bool) -> Fut,
    Fut: Future<Output = Option<WebNode<N>>>,
{
    let mut roots: Vec<WebRoot<N>> = Vec::new();
    let mut visited = 0usize;
    let mut truncated = false;
    let mut level: Vec<(N, At)> = vec![(frame, At::Same { depth: 0 })];
    while !level.is_empty() {
        let room = limits.max_nodes.saturating_sub(visited);
        if level.len() > room {
            level.truncate(room);
            truncated = true;
        }
        if level.is_empty() {
            break;
        }
        let read = &read;
        let mut results = stream::iter(level.into_iter().map(|(n, at)| async move {
            let d = read(n.clone(), matches!(at, At::Cross { .. })).await;
            (n, at, d)
        }))
        .buffered(limits.concurrency.max(1));
        let mut next = Vec::new();
        while let Some((n, at, data)) = results.next().await {
            visited += 1;
            let Some(d) = data else { continue };
            match at {
                At::Same { depth } => {
                    let descend = depth == 0 || d.showing;
                    for c in d.children {
                        if c.bus() != app_bus {
                            let idx = roots.len();
                            roots.push(WebRoot {
                                root: None,
                                crossing: c.clone(),
                                view: n.clone(),
                            });
                            next.push((c, At::Cross { idx, level: 0 }));
                        } else if descend {
                            next.push((c, At::Same { depth: depth + 1 }));
                        }
                    }
                }
                At::Cross { idx, level } => {
                    if roots[idx].root.is_some() {
                        continue;
                    }
                    if d.has_collection {
                        roots[idx].root = Some(n);
                        continue;
                    }
                    if level < CROSS_DEPTH {
                        next.extend(d.children.into_iter().map(|c| {
                            (
                                c,
                                At::Cross {
                                    idx,
                                    level: level + 1,
                                },
                            )
                        }));
                    }
                }
            }
        }
        // Queued nodes of crossings that already have a root are not needed.
        next.retain(|(_, at)| match at {
            At::Cross { idx, .. } => roots[*idx].root.is_none(),
            At::Same { .. } => true,
        });
        if Instant::now() >= limits.deadline && !next.is_empty() {
            truncated = true;
            break;
        }
        level = next;
    }
    Discovery {
        roots,
        visited,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct N(&'static str, u32);

    impl OnBus for N {
        fn bus(&self) -> &str {
            self.0
        }
    }

    const UI: &str = ":1.411";
    const WEB: &str = ":1.412";
    const WEB2: &str = ":1.413";

    /// node → (showing, has_collection, children)
    type Tree = HashMap<N, (bool, bool, Vec<N>)>;

    fn limits() -> DiscoverLimits {
        DiscoverLimits {
            max_nodes: 1500,
            concurrency: 16,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }

    async fn run(t: &Tree, l: DiscoverLimits) -> Discovery<N> {
        discover(N(UI, 0), UI, l, |n, _| async move {
            t.get(&n).map(|(s, col, c)| WebNode {
                showing: *s,
                has_collection: *col,
                children: c.clone(),
            })
        })
        .await
    }

    /// wodeo: frame → filler → WebKitWebView (no states) → plug → scroll pane.
    fn wodeo() -> Tree {
        HashMap::from([
            (N(UI, 0), (true, true, vec![N(UI, 1)])),
            (N(UI, 1), (true, true, vec![N(UI, 2)])),
            (N(UI, 2), (false, true, vec![N(WEB, 10)])),
            (N(WEB, 10), (false, false, vec![N(WEB, 11)])),
            (N(WEB, 11), (true, true, vec![N(WEB, 12)])),
            (N(WEB, 12), (true, true, vec![])),
        ])
    }

    #[tokio::test]
    async fn wodeo_one_root() {
        let d = run(&wodeo(), limits()).await;
        assert_eq!(
            d.roots,
            [WebRoot {
                root: Some(N(WEB, 11)),
                crossing: N(WEB, 10),
                view: N(UI, 2),
            }]
        );
        assert!(!d.truncated);
        // The document below the root is never read.
        assert_eq!(d.visited, 5);
    }

    #[tokio::test]
    async fn two_web_views() {
        let mut t = wodeo();
        t.insert(N(UI, 1), (true, true, vec![N(UI, 2), N(UI, 3)]));
        t.insert(N(UI, 3), (false, true, vec![N(WEB2, 20)]));
        t.insert(N(WEB2, 20), (false, false, vec![N(WEB2, 21)]));
        t.insert(N(WEB2, 21), (true, true, vec![]));
        let d = run(&t, limits()).await;
        let roots: Vec<_> = d.roots.iter().map(|r| r.root).collect();
        assert_eq!(roots, [Some(N(WEB, 11)), Some(N(WEB2, 21))]);
        assert_eq!(d.roots[1].view, N(UI, 3));
    }

    #[tokio::test]
    async fn no_collection_within_depth() {
        let mut t = wodeo();
        // Chain of 6 cross-bus nodes without Collection, then one with it.
        t.insert(N(WEB, 10), (false, false, vec![N(WEB, 30)]));
        for i in 30..35 {
            t.insert(N(WEB, i), (true, false, vec![N(WEB, i + 1)]));
        }
        t.insert(N(WEB, 35), (true, true, vec![]));
        let d = run(&t, limits()).await;
        assert_eq!(d.roots.len(), 1);
        assert_eq!(d.roots[0].root, None);
        assert_eq!(d.roots[0].crossing, N(WEB, 10));
        assert!(!d.truncated);
    }

    #[tokio::test]
    async fn hidden_same_bus_not_descended() {
        let mut t = wodeo();
        t.insert(N(UI, 1), (true, true, vec![N(UI, 2), N(UI, 4)]));
        t.insert(N(UI, 4), (false, true, vec![N(UI, 5)]));
        t.insert(N(UI, 5), (true, true, vec![N(UI, 6)]));
        t.insert(N(UI, 6), (true, true, vec![]));
        let seen = std::sync::Mutex::new(Vec::new());
        let (t, s) = (&t, &seen);
        let d = discover(N(UI, 0), UI, limits(), |n, _| async move {
            s.lock().unwrap().push(n);
            t.get(&n).map(|(sh, col, c)| WebNode {
                showing: *sh,
                has_collection: *col,
                children: c.clone(),
            })
        })
        .await;
        assert_eq!(d.roots.len(), 1);
        let seen = seen.into_inner().unwrap();
        assert!(seen.contains(&N(UI, 4)));
        assert!(!seen.contains(&N(UI, 5)));
    }

    #[tokio::test]
    async fn budget_truncates() {
        let mut l = limits();
        l.max_nodes = 4;
        let d = run(&wodeo(), l).await;
        assert!(d.truncated);
        assert_eq!(d.visited, 4);
        assert_eq!(d.roots.len(), 1);
        assert_eq!(d.roots[0].root, None);
    }
}
