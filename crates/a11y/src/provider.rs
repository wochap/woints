//! `AtspiProvider`: pairing, strategies, extents and filters (design D13–D19).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream;
use zbus::Connection;

use woints_core::geom::{Global, Rect, Size};
use woints_core::model::{ActionKind, Element, ElementId, ElementSource, Target, Timings};
use woints_core::ports::{Budget, CollectReport, Collected, ElementProvider, ProviderError};
use woints_core::profile::{Strategy, ToolkitProfile, base_filters};
use woints_core::proto::ProviderStatus;

use crate::conn::{A11yConnection, is_transport};
use crate::dbus::{self, Obj};
use crate::filter::{Candidate, ParentInfo, keep};
use crate::pair::{AppInfo, FrameInfo, FrameReason, pick_app, pick_frame};
use crate::rule::{MatchRule, has_state, is_web_content, state};
use crate::walk::{self, NodeData, WalkLimits};

/// Extra time allowed past the budget for in-flight replies before a phase is abandoned.
const GRACE: Duration = Duration::from_millis(40);
/// How far up `CollapsedComboPopup` looks for a combo box.
const COMBO_DEPTH: usize = 3;
/// Ancestor depth checked when rejecting Collection matches inside web content.
const WEB_ANCESTOR_DEPTH: usize = 40;

/// Applications by bus name, valid for one connection generation.
#[derive(Default)]
pub struct AppCache {
    generation: u64,
    apps: HashMap<String, AppInfo>,
}

impl AppCache {
    pub fn app(&self, bus: &str) -> Option<&AppInfo> {
        self.apps.get(bus)
    }
}

pub type SharedApps = Arc<Mutex<AppCache>>;

pub struct AtspiProvider {
    conn: Arc<A11yConnection>,
    apps: SharedApps,
    pub max_nodes: usize,
    pub max_depth: usize,
}

/// Candidate before extents: object, parent if known, role if known.
struct Pending {
    obj: Obj,
    parent: Option<Obj>,
    role: Option<String>,
}

struct Read {
    obj: Obj,
    parent: Option<Obj>,
    role: String,
    name: String,
    states: Vec<u32>,
    rect: (i32, i32, i32, i32),
    n_actions: i32,
}

#[derive(Clone)]
struct Ancestor {
    role: String,
    states: Vec<u32>,
    rect: Option<(i32, i32, i32, i32)>,
    parent: Option<Obj>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl AtspiProvider {
    pub fn new(conn: Arc<A11yConnection>) -> Self {
        Self {
            conn,
            apps: Arc::default(),
            max_nodes: walk::DEFAULT_MAX_NODES,
            max_depth: walk::DEFAULT_MAX_DEPTH,
        }
    }

    pub fn connection(&self) -> Arc<A11yConnection> {
        self.conn.clone()
    }

    pub fn apps_cache(&self) -> SharedApps {
        self.apps.clone()
    }

    pub fn status(&self) -> ProviderStatus {
        let (available, last_error) = self.conn.status();
        ProviderStatus {
            name: "atspi".into(),
            available,
            last_error,
        }
    }

    /// Applications under the Registry root. App facts are cached per bus
    /// name and dropped on `NameOwnerChanged`.
    pub async fn apps(
        &self,
        c: &Connection,
        generation: u64,
    ) -> Result<Vec<AppInfo>, ProviderError> {
        let roots = match dbus::registry_apps(c).await {
            Ok(r) => r,
            Err(e) => {
                if is_transport(&e) {
                    self.conn.invalidate(generation, &e.to_string());
                }
                return Err(ProviderError::Unavailable(format!("registry: {e}")));
            }
        };
        let fresh = {
            let mut cache = lock(&self.apps);
            if cache.generation != generation {
                cache.generation = generation;
                cache.apps.clear();
                watch_owners(c.clone(), self.apps.clone(), generation);
            }
            roots
                .iter()
                .filter(|o| !cache.apps.contains_key(&o.bus))
                .cloned()
                .collect::<Vec<_>>()
        };
        let infos: Vec<AppInfo> = stream::iter(fresh.into_iter().map(|o| app_info(c, o)))
            .buffer_unordered(walk::CONCURRENCY)
            .collect()
            .await;
        let mut cache = lock(&self.apps);
        for i in infos {
            cache.apps.insert(i.bus_name.clone(), i);
        }
        Ok(roots
            .iter()
            .filter_map(|o| cache.apps.get(&o.bus).cloned())
            .collect())
    }

    pub async fn pair(
        &self,
        c: &Connection,
        generation: u64,
        target: &Target,
        report: &mut CollectReport,
    ) -> Result<Option<(AppInfo, Obj)>, ProviderError> {
        let apps = self.apps(c, generation).await?;
        let Some((i, reason)) = pick_app(&apps, target.pid, &target.class) else {
            report.paired = Some("no-a11y-app".into());
            return Ok(None);
        };
        let app = apps[i].clone();
        report.paired = Some(reason.as_str().into());
        report.toolkit = Some(format!("{} {}", app.toolkit, app.version).trim().to_owned());
        let frames = dbus::children(c, &Obj::root(&app.bus_name))
            .await
            .map_err(|e| ProviderError::Other(format!("frames of {}: {e}", app.bus_name)))?;
        let infos: Vec<FrameInfo> = futures::future::join_all(frames.iter().map(|f| async move {
            let (st, ext) = futures::join!(dbus::state(c, f), dbus::extents(c, f));
            FrameInfo {
                active: st.is_ok_and(|s| has_state(&s, state::ACTIVE)),
                size: ext.ok().map(|(_, _, w, h)| Size { w, h }),
            }
        }))
        .await;
        let size = Size {
            w: target.rect.w,
            h: target.rect.h,
        };
        let Some((fi, freason)) = pick_frame(&infos, size) else {
            report.reasons.push("no-frame".into());
            return Ok(None);
        };
        if freason == FrameReason::Guess {
            report.paired = Some("frame-guess".into());
        }
        Ok(Some((app, frames[fi].clone())))
    }

    async fn collection(
        &self,
        c: &Connection,
        frame: &Obj,
        profile: &ToolkitProfile,
        report: &mut CollectReport,
    ) -> Option<Vec<Pending>> {
        match dbus::interfaces(c, frame).await {
            Ok(ifaces) if ifaces.iter().any(|i| i == dbus::COLLECTION) => {}
            Ok(_) => {
                report.reasons.push("collection-unsupported".into());
                return None;
            }
            Err(e) => {
                report.reasons.push(format!("collection-error: {e}"));
                return None;
            }
        }
        let found = match dbus::get_matches(c, frame, &MatchRule::interactive()).await {
            Ok(f) => f,
            Err(e) => {
                report.reasons.push(format!("collection-error: {e}"));
                return None;
            }
        };
        if found.is_empty() {
            report.reasons.push("collection-empty".into());
            return None;
        }
        let found = if profile.skip_web_content {
            let futs: Vec<_> = found.iter().map(|o| in_web_content(c, o)).collect();
            let flags: Vec<bool> = stream::iter(futs)
                .buffered(walk::CONCURRENCY)
                .collect()
                .await;
            found
                .into_iter()
                .zip(flags)
                .filter(|(_, web)| !web)
                .map(|(o, _)| o)
                .collect()
        } else {
            found
        };
        Some(
            found
                .into_iter()
                .map(|obj| Pending {
                    obj,
                    parent: None,
                    role: None,
                })
                .collect(),
        )
    }

    async fn tree_walk(
        &self,
        c: &Connection,
        frame: &Obj,
        profile: &ToolkitProfile,
        budget: Budget,
        report: &mut CollectReport,
    ) -> Vec<Pending> {
        let limits = WalkLimits {
            max_nodes: self.max_nodes,
            max_depth: self.max_depth,
            concurrency: walk::CONCURRENCY,
            deadline: budget.deadline,
            skip_web_content: profile.skip_web_content,
        };
        let r = walk::walk(frame.clone(), limits, |o: Obj| async move {
            let (role, st, kids) = futures::join!(
                dbus::role_name(c, &o),
                dbus::state(c, &o),
                dbus::children(c, &o)
            );
            let st = st.ok()?;
            Some(NodeData {
                role: role.ok()?,
                showing: has_state(&st, state::SHOWING) && has_state(&st, state::VISIBLE),
                children: kids.unwrap_or_default(),
            })
        })
        .await;
        tracing::debug!(
            visited = r.visited,
            found = r.found.len(),
            truncated = r.truncated,
            "walk"
        );
        report.truncated |= r.truncated;
        if r.found.is_empty() {
            report.reasons.push("walk-empty".into());
        }
        r.found
            .into_iter()
            .map(|f| Pending {
                obj: f.node,
                parent: f.parent,
                role: Some(f.role),
            })
            .collect()
    }

    async fn ancestor(
        &self,
        c: &Connection,
        o: &Obj,
        cache: &Mutex<HashMap<Obj, Ancestor>>,
    ) -> Option<Ancestor> {
        if let Some(a) = lock(cache).get(o) {
            return Some(a.clone());
        }
        let (role, st, ext, parent) = futures::join!(
            dbus::role_name(c, o),
            dbus::state(c, o),
            dbus::extents(c, o),
            dbus::parent(c, o)
        );
        let a = Ancestor {
            role: role.ok()?,
            states: st.unwrap_or_default(),
            rect: ext.ok(),
            parent: parent.ok().flatten(),
        };
        lock(cache).insert(o.clone(), a.clone());
        Some(a)
    }

    /// Run every phase for one target.
    pub async fn collect_target(
        &self,
        target: &Target,
        budget: Budget,
    ) -> Result<Collected, ProviderError> {
        let mut timings = Timings::default();
        let mut report = CollectReport {
            provider: "atspi".into(),
            ..Default::default()
        };
        let hard = tokio::time::Instant::from_std(budget.deadline + GRACE);

        // collect.connect
        let t = Instant::now();
        let (c, generation) = self.conn.get().await?;
        timings.record("collect.connect", t.elapsed());

        // collect.match
        let t = Instant::now();
        let matched = tokio::time::timeout_at(hard, async {
            let Some((app, frame)) = self.pair(&c, generation, target, &mut report).await? else {
                return Ok::<_, ProviderError>(None);
            };
            let profile = ToolkitProfile::lookup(&app.toolkit, &app.version, &target.class);
            let mut pending = None;
            for s in &profile.strategies {
                pending = match s {
                    Strategy::Collection => {
                        self.collection(&c, &frame, &profile, &mut report).await
                    }
                    Strategy::TreeWalk => Some(
                        self.tree_walk(&c, &frame, &profile, budget, &mut report)
                            .await,
                    ),
                };
                if pending.as_ref().is_some_and(|p| !p.is_empty()) {
                    report.used = Some(s.as_str().into());
                    break;
                }
            }
            Ok(Some((profile, pending.unwrap_or_default())))
        })
        .await;
        timings.record("collect.match", t.elapsed());
        let (profile, pending) = match matched {
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => {
                return Ok(Collected {
                    elements: Vec::new(),
                    report,
                    timings,
                });
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                report.truncated = true;
                report.reasons.push("match-timeout".into());
                return Ok(Collected {
                    elements: Vec::new(),
                    report,
                    timings,
                });
            }
        };

        // collect.extents
        let t = Instant::now();
        let reads: Vec<Read> = {
            let c = &c;
            let fut = stream::iter(pending.into_iter().map(|p| async move {
                let role_fut = async {
                    match &p.role {
                        Some(r) => Ok(r.clone()),
                        None => dbus::role_name(c, &p.obj).await,
                    }
                };
                let (ext, st, role, name, ifaces) = futures::join!(
                    dbus::extents(c, &p.obj),
                    dbus::state(c, &p.obj),
                    role_fut,
                    dbus::name(c, &p.obj),
                    dbus::interfaces(c, &p.obj)
                );
                let n_actions = if ifaces.is_ok_and(|i| i.iter().any(|i| i == dbus::ACTION)) {
                    dbus::n_actions(c, &p.obj).await.unwrap_or(0)
                } else {
                    0
                };
                Some(Read {
                    rect: ext.ok()?,
                    states: st.ok()?,
                    role: role.ok()?,
                    name: name.unwrap_or_default(),
                    n_actions,
                    obj: p.obj,
                    parent: p.parent,
                })
            }))
            .buffered(walk::CONCURRENCY)
            .filter_map(|r| async move { r })
            .collect::<Vec<_>>();
            match tokio::time::timeout_at(hard, fut).await {
                Ok(r) => r,
                Err(_) => {
                    report.truncated = true;
                    report.reasons.push("extents-timeout".into());
                    Vec::new()
                }
            }
        };
        timings.record("collect.extents", t.elapsed());

        // collect.filter
        let t = Instant::now();
        let origin = target.rect.origin();
        let global = |(x, y, w, h): (i32, i32, i32, i32)| -> Rect<Global> {
            Rect::new(origin.x + x, origin.y + y, w, h)
        };
        let mut filters = base_filters();
        filters.extend(profile.phantom_filters.iter().cloned());
        let needs_parent = |role: &str| role == "list item" || role == "menu item";
        let cache = Mutex::new(HashMap::new());
        let cands: Vec<Candidate> = {
            let (c, cache, global) = (&c, &cache, &global);
            let futs: Vec<_> = reads
                .iter()
                .map(|r| async move {
                    let mut cand = Candidate {
                        rect: global(r.rect),
                        role: r.role.clone(),
                        parent: None,
                        combo_expanded: None,
                    };
                    if !needs_parent(&r.role) {
                        return cand;
                    }
                    let mut next = match &r.parent {
                        Some(p) => Some(p.clone()),
                        None => dbus::parent(c, &r.obj).await.ok().flatten(),
                    };
                    for level in 0..COMBO_DEPTH {
                        let Some(o) = next.take() else { break };
                        let Some(a) = self.ancestor(c, &o, cache).await else {
                            break;
                        };
                        if level == 0 {
                            cand.parent = a.rect.map(|e| ParentInfo {
                                role: a.role.clone(),
                                rect: global(e),
                            });
                        }
                        if a.role == "combo box" {
                            cand.combo_expanded = Some(has_state(&a.states, state::EXPANDED));
                            break;
                        }
                        next = a.parent;
                    }
                    cand
                })
                .collect();
            stream::iter(futs)
                .buffered(walk::CONCURRENCY)
                .collect()
                .await
        };
        let kept = keep(&cands, &filters, &target.rect);
        let elements: Vec<Element> = kept
            .into_iter()
            .enumerate()
            .map(|(n, i)| {
                let r = &reads[i];
                Element {
                    id: ElementId(n as u32 + 1),
                    rect: cands[i].rect,
                    role: r.role.clone(),
                    name: r.name.clone(),
                    source: ElementSource::Atspi,
                    actions: action_kinds(&r.role, &r.states, r.n_actions),
                    confidence: 1.0,
                    handle: Some(r.obj.handle()),
                }
            })
            .collect();
        timings.record("collect.filter", t.elapsed());
        Ok(Collected {
            elements,
            report,
            timings,
        })
    }
}

impl ElementProvider for AtspiProvider {
    fn name(&self) -> &'static str {
        "atspi"
    }

    async fn collect(&self, target: &Target, budget: Budget) -> Result<Collected, ProviderError> {
        self.collect_target(target, budget).await
    }
}

/// `Activate` when the element has actions, `Focus` (first) for text
/// entries, `Pointer` when neither applies.
pub fn action_kinds(role: &str, states: &[u32], n_actions: i32) -> Vec<ActionKind> {
    let mut v = Vec::new();
    if role == "entry" || role == "password text" || has_state(states, state::EDITABLE) {
        v.push(ActionKind::Focus);
    }
    if n_actions > 0 {
        v.push(ActionKind::Activate);
    }
    if v.is_empty() {
        v.push(ActionKind::Pointer);
    }
    v
}

async fn app_info(c: &Connection, root: Obj) -> AppInfo {
    let app = Obj::root(&root.bus);
    let (name, toolkit, version, pid) = futures::join!(
        dbus::name(c, &app),
        dbus::prop_string(c, &app, dbus::APPLICATION, "ToolkitName"),
        dbus::prop_string(c, &app, dbus::APPLICATION, "Version"),
        dbus::pid_of(c, &root.bus)
    );
    AppInfo {
        bus_name: root.bus,
        name: name.unwrap_or_default(),
        toolkit: toolkit.unwrap_or_default(),
        version: version.unwrap_or_default(),
        pid: pid.ok(),
    }
}

async fn in_web_content(c: &Connection, o: &Obj) -> bool {
    let mut cur = dbus::parent(c, o).await.ok().flatten();
    for _ in 0..WEB_ANCESTOR_DEPTH {
        let Some(p) = cur else { return false };
        if dbus::role_name(c, &p)
            .await
            .is_ok_and(|r| is_web_content(&r))
        {
            return true;
        }
        cur = dbus::parent(c, &p).await.ok().flatten();
    }
    false
}

/// Drop cached app facts when a bus name changes owner.
fn watch_owners(c: Connection, apps: SharedApps, generation: u64) {
    tokio::spawn(async move {
        let proxy = match zbus::fdo::DBusProxy::new(&c).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("NameOwnerChanged watch: {e}");
                return;
            }
        };
        let Ok(mut changes) = proxy.receive_name_owner_changed().await else {
            return;
        };
        while let Some(sig) = changes.next().await {
            let Ok(args) = sig.args() else { continue };
            let mut cache = lock(&apps);
            if cache.generation != generation {
                return;
            }
            if cache.apps.remove(args.name().as_str()).is_some() {
                tracing::debug!(bus = %args.name(), "a11y app gone");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_tags() {
        assert_eq!(action_kinds("push button", &[], 1), [ActionKind::Activate]);
        assert_eq!(
            action_kinds("entry", &[], 1),
            [ActionKind::Focus, ActionKind::Activate]
        );
        assert_eq!(action_kinds("password text", &[], 0), [ActionKind::Focus]);
        let editable = [1u32 << state::EDITABLE, 0];
        assert_eq!(action_kinds("combo box", &editable, 0), [ActionKind::Focus]);
        assert_eq!(action_kinds("list item", &[], 0), [ActionKind::Pointer]);
    }
}
