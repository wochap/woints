//! Quickshell provider (design D15): discovers quickshell instances with
//! `qs list`, probes each for the `woints` IPC target of the Woints QML
//! module, lists hintable items once per instance per request and invokes
//! them. Forking `qs` is the only sanctioned helper fork in the hot path.

pub mod parse;
pub mod proc;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use woints_core::config::glob;
use woints_core::model::{Action, Element, Target, Timings};
use woints_core::ports::{
    Budget, CollectReport, Collected, ElementProvider, ProviderError, QS_IGNORED,
};
use woints_core::proto::ProviderStatus;

pub use parse::{Instance, Item, PROTOCOL, split_handle};
pub use proc::{RunError, spawn_count};

/// Time allowed for `qs list` and `qs ipc show` during discovery.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Runs `qs` with arguments; swapped out in tests.
pub trait Runner: Send + Sync {
    fn run(
        &self,
        args: Vec<String>,
        timeout: Duration,
    ) -> impl Future<Output = Result<String, RunError>> + Send;
}

/// The real `qs` binary.
pub struct QsBinary(pub String);

impl Default for QsBinary {
    fn default() -> Self {
        Self("qs".into())
    }
}

impl Runner for QsBinary {
    async fn run(&self, args: Vec<String>, timeout: Duration) -> Result<String, RunError> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        proc::run(&self.0, &a, timeout).await
    }
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

#[derive(Debug, Clone)]
struct Known {
    id: String,
    config_path: String,
    module: bool,
    /// `qs ipc show` ran for this instance.
    probed: bool,
}

fn is_ignored(ignore: &[String], config_path: &str) -> bool {
    ignore.iter().any(|g| glob(g, config_path))
}

/// What `resolve` knows about a layer pid.
#[derive(Debug, Clone, PartialEq)]
enum Resolved {
    /// Instance id with the Woints module.
    Module(String),
    Ignored,
}

#[derive(Default)]
struct Cache {
    /// Quickshell instances by pid, with their probe result.
    instances: HashMap<u32, Known>,
    /// Layer pids `qs list` did not report: not quickshell.
    foreign: HashSet<u32>,
    last_error: Option<String>,
}

/// One instance as `doctor` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceReport {
    pub id: String,
    pub pid: u32,
    pub config_path: String,
    pub module: bool,
    /// Matched `providers.quickshell.ignore_configs`; not probed.
    pub ignored: bool,
    /// Items listed, or the error.
    pub items: Result<usize, String>,
}

/// Elements per target plus the provider's stages.
#[derive(Debug, Default)]
pub struct LayerCollect {
    pub per_target: Vec<Collected>,
    pub timings: Timings,
}

pub struct QuickshellIpcProvider<R: Runner = QsBinary> {
    runner: R,
    cache: Mutex<Cache>,
    /// Budget for one `list` call.
    pub budget: Duration,
}

impl QuickshellIpcProvider<QsBinary> {
    pub fn new(budget: Duration) -> Self {
        Self::with_runner(QsBinary::default(), budget)
    }
}

impl<R: Runner> QuickshellIpcProvider<R> {
    pub fn with_runner(runner: R, budget: Duration) -> Self {
        Self {
            runner,
            cache: Mutex::new(Cache::default()),
            budget,
        }
    }

    pub async fn status(&self) -> ProviderStatus {
        let c = self.cache.lock().await;
        ProviderStatus {
            name: "quickshell".into(),
            available: c.instances.values().any(|k| k.module),
            last_error: c.last_error.clone(),
        }
    }

    /// Re-run `qs list`. Instances that vanished are dropped; pids not listed
    /// become foreign. New instances are probed by `probe_pending`.
    async fn discover(&self, cache: &mut Cache, pids: &[u32]) {
        let listed = match self
            .runner
            .run(args(&["list", "--all", "--json"]), DISCOVERY_TIMEOUT)
            .await
            .map_err(|e| e.to_string())
            .and_then(|s| parse::parse_instances(&s))
        {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("quickshell discovery: {e}");
                cache.last_error = Some(e);
                // Avoid forking on every request when `qs` is missing.
                cache.foreign.extend(pids);
                return;
            }
        };
        cache
            .instances
            .retain(|pid, _| listed.iter().any(|i| i.pid == *pid));
        for inst in &listed {
            if cache
                .instances
                .get(&inst.pid)
                .is_some_and(|k| k.id == inst.id)
            {
                continue;
            }
            cache.instances.insert(
                inst.pid,
                Known {
                    id: inst.id.clone(),
                    config_path: inst.config_path.clone(),
                    module: false,
                    probed: false,
                },
            );
        }
        for &p in pids {
            if !cache.instances.contains_key(&p) {
                cache.foreign.insert(p);
            }
        }
        cache.last_error = None;
    }

    /// Run `qs ipc show` for every cached instance not yet probed and not
    /// ignored.
    async fn probe_pending(&self, cache: &mut Cache, ignore: &[String]) {
        let pending: Vec<u32> = cache
            .instances
            .iter()
            .filter(|(_, k)| !k.probed && !is_ignored(ignore, &k.config_path))
            .map(|(p, _)| *p)
            .collect();
        for pid in pending {
            let Some(k) = cache.instances.get_mut(&pid) else {
                continue;
            };
            k.module = match self
                .runner
                .run(
                    args(&["ipc", "--pid", &pid.to_string(), "show"]),
                    DISCOVERY_TIMEOUT,
                )
                .await
            {
                Ok(show) => parse::has_module(&show),
                Err(e) => {
                    tracing::debug!(pid, "qs ipc show: {e}");
                    false
                }
            };
            k.probed = true;
            if k.module {
                tracing::info!(id = %k.id, pid, "quickshell instance with the Woints module");
            } else {
                tracing::info!(
                    id = %k.id,
                    pid,
                    config = %k.config_path,
                    "quickshell instance without the Woints module; its layers fall back to vision"
                );
            }
        }
    }

    /// Instances for `pids`: the module id, or ignored. Discovers when a pid
    /// is new and probes instances that are no longer ignored.
    async fn resolve(&self, pids: &[u32], ignore: &[String]) -> HashMap<u32, Resolved> {
        let mut cache = self.cache.lock().await;
        let unknown = pids
            .iter()
            .any(|p| !cache.instances.contains_key(p) && !cache.foreign.contains(p));
        if unknown {
            self.discover(&mut cache, pids).await;
        }
        self.probe_pending(&mut cache, ignore).await;
        pids.iter()
            .filter_map(|p| {
                let k = cache.instances.get(p)?;
                if is_ignored(ignore, &k.config_path) {
                    Some((*p, Resolved::Ignored))
                } else {
                    k.module.then(|| (*p, Resolved::Module(k.id.clone())))
                }
            })
            .collect()
    }

    async fn list(&self, id: &str, timeout: Duration) -> Result<Vec<Item>, String> {
        let out = self
            .runner
            .run(
                args(&["ipc", "--id", id, "call", parse::TARGET, "list"]),
                timeout,
            )
            .await
            .map_err(|e| format!("qs ipc call woints list: {e}"))?;
        parse::parse_list(&out)
    }

    /// Forget an instance after a failed call so the next request
    /// rediscovers it (quickshell restarted or exited).
    async fn forget(&self, id: &str) {
        let mut c = self.cache.lock().await;
        c.instances.retain(|_, k| k.id != id);
    }

    /// Elements for every layer target, one `list` per instance; stage `qs.list`.
    pub async fn collect_layers(
        &self,
        targets: &[&Target],
        budget: Budget,
        ignore: &[String],
    ) -> LayerCollect {
        let t = Instant::now();
        let mut pids: Vec<u32> = targets
            .iter()
            .filter(|t| t.surface.is_layer())
            .filter_map(|t| t.pid)
            .collect();
        pids.sort_unstable();
        pids.dedup();
        let mut timings = Timings::default();
        // Discovery (`qs list`, `qs ipc show`) is cached per pid after the first
        // request; do not charge it against the `list` timeout, otherwise the
        // first request after startup always times out.
        let remaining = budget.deadline.saturating_duration_since(Instant::now());
        let ids = if pids.is_empty() {
            HashMap::new()
        } else {
            self.resolve(&pids, ignore).await
        };
        let mut uniq: Vec<&String> = ids
            .values()
            .filter_map(|r| match r {
                Resolved::Module(id) => Some(id),
                Resolved::Ignored => None,
            })
            .collect();
        uniq.sort();
        uniq.dedup();
        let timeout = self.budget.min(remaining);
        let lists = futures::future::join_all(
            uniq.iter()
                .map(|id| async move { (id.to_string(), self.list(id, timeout).await) }),
        )
        .await;
        let mut items: HashMap<String, Result<Vec<Item>, String>> = HashMap::new();
        for (id, r) in lists {
            if let Err(e) = &r {
                tracing::warn!(instance = %id, "{e}");
                self.forget(&id).await;
                self.cache.lock().await.last_error = Some(e.clone());
            }
            items.insert(id, r);
        }
        if !uniq.is_empty() {
            timings.record("qs.list", t.elapsed());
        }
        let per_target = targets
            .iter()
            .map(|target| {
                let mut report = CollectReport {
                    provider: "quickshell".into(),
                    ..Default::default()
                };
                let elements = match target.pid.and_then(|p| ids.get(&p)) {
                    None => {
                        report.reasons.push("no-module".into());
                        Vec::new()
                    }
                    Some(Resolved::Ignored) => {
                        report.reasons.push(QS_IGNORED.into());
                        Vec::new()
                    }
                    Some(Resolved::Module(id)) => match items.get(id) {
                        Some(Ok(list)) => {
                            report.paired = Some("pid".into());
                            parse::elements_for(target, id, list)
                        }
                        Some(Err(e)) => {
                            report.reasons.push(e.clone());
                            report.truncated = e.contains("timed out");
                            Vec::new()
                        }
                        None => Vec::new(),
                    },
                };
                Collected {
                    elements,
                    report,
                    timings: Timings::default(),
                }
            })
            .collect();
        LayerCollect {
            per_target,
            timings,
        }
    }

    /// `invoke(id, action)` on the module. `Ok` only for an `"ok"` reply.
    pub async fn invoke(&self, element: &Element, action: Action) -> Result<(), String> {
        let (instance, item) = element
            .handle
            .as_deref()
            .and_then(split_handle)
            .ok_or("not a quickshell element")?;
        let out = self
            .runner
            .run(
                args(&[
                    "ipc",
                    "--id",
                    instance,
                    "call",
                    parse::TARGET,
                    "invoke",
                    item,
                    action.as_str(),
                ]),
                self.budget.max(Duration::from_millis(200)),
            )
            .await
            .map_err(|e| e.to_string())?;
        match out.trim() {
            "ok" => Ok(()),
            other => Err(format!("invoke returned \"{other}\"")),
        }
    }

    /// Fresh discovery and a `list` per instance, for `doctor`.
    pub async fn doctor(&self, ignore: &[String]) -> Result<Vec<InstanceReport>, String> {
        let mut cache = self.cache.lock().await;
        cache.instances.clear();
        cache.foreign.clear();
        self.discover(&mut cache, &[]).await;
        if let Some(e) = &cache.last_error {
            return Err(e.clone());
        }
        self.probe_pending(&mut cache, ignore).await;
        let known: Vec<(u32, Known)> = cache
            .instances
            .iter()
            .map(|(p, k)| (*p, k.clone()))
            .collect();
        drop(cache);
        let mut out = Vec::new();
        for (pid, k) in known {
            let ignored = is_ignored(ignore, &k.config_path);
            let items = if ignored {
                Ok(0)
            } else if k.module {
                self.list(&k.id, DISCOVERY_TIMEOUT).await.map(|i| i.len())
            } else {
                Err("Woints module not imported".into())
            };
            out.push(InstanceReport {
                id: k.id,
                pid,
                config_path: k.config_path,
                module: k.module,
                ignored,
                items,
            });
        }
        out.sort_by_key(|r| r.pid);
        Ok(out)
    }
}

impl<R: Runner> ElementProvider for QuickshellIpcProvider<R> {
    fn name(&self) -> &'static str {
        "quickshell"
    }

    async fn collect(&self, target: &Target, budget: Budget) -> Result<Collected, ProviderError> {
        let mut c = self.collect_layers(&[target], budget, &[]).await;
        let mut one = c.per_target.pop().unwrap_or_default();
        one.timings = c.timings;
        Ok(one)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use woints_core::geom::{OutputId, Rect};
    use woints_core::model::Surface;

    const SHOW_MODULE: &str = include_str!("../tests/fixtures/show-module.txt");
    const SHOW: &str = include_str!("../tests/fixtures/show-no-module.txt");
    const ITEMS: &str = include_str!("../tests/fixtures/items.json");

    /// Fake `qs` with one instance per entry of `instances` (pid, id, module).
    struct Fake {
        instances: StdMutex<Vec<(u32, &'static str, bool)>>,
        calls: StdMutex<Vec<String>>,
        list_reply: StdMutex<Result<String, RunError>>,
    }

    impl Fake {
        fn new(instances: Vec<(u32, &'static str, bool)>) -> Self {
            Self {
                instances: StdMutex::new(instances),
                calls: StdMutex::new(Vec::new()),
                list_reply: StdMutex::new(Ok(ITEMS.into())),
            }
        }
        fn calls(&self, prefix: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| c.starts_with(prefix))
                .count()
        }
    }

    impl Runner for &Fake {
        async fn run(&self, a: Vec<String>, _: Duration) -> Result<String, RunError> {
            let line = a.join(" ");
            self.calls.lock().unwrap().push(line.clone());
            let inst = self.instances.lock().unwrap().clone();
            if line == "list --all --json" {
                let v: Vec<_> = inst
                    .iter()
                    .map(|(pid, id, _)| serde_json::json!({"id": id, "pid": pid, "config_path": format!("/x/{id}.qml")}))
                    .collect();
                return Ok(serde_json::to_string(&v).unwrap());
            }
            if let Some(rest) = line.strip_prefix("ipc --pid ") {
                let pid: u32 = rest.split(' ').next().unwrap().parse().unwrap();
                let module = inst.iter().any(|(p, _, m)| *p == pid && *m);
                return Ok(if module { SHOW_MODULE } else { SHOW }.into());
            }
            if line.ends_with("call woints list") {
                return self.list_reply.lock().unwrap().clone();
            }
            if line.contains("invoke") {
                return Ok(if line.ends_with("click") {
                    "ok\n"
                } else {
                    "unhandled\n"
                }
                .into());
            }
            Err(RunError::Failed(format!("unexpected: {line}")))
        }
    }

    fn layer(ns: &str, pid: u32) -> Target {
        Target {
            surface: Surface::Layer {
                address: "0x1".into(),
                namespace: ns.into(),
            },
            rect: Rect::new(0, 0, 1440, 80),
            output: OutputId("eDP-1".into()),
            pid: Some(pid),
            class: ns.into(),
        }
    }

    fn budget() -> Budget {
        Budget {
            deadline: Instant::now() + Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn one_list_per_instance_and_partition() {
        let fake = Fake::new(vec![(10, "shell", true), (11, "lock", false)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let bar = layer("quickshell:bar", 10);
        let keys = layer("quickshell:which-keys", 10);
        let lock = layer("quickshell:lock", 11);
        let c = p.collect_layers(&[&bar, &keys, &lock], budget(), &[]).await;
        assert_eq!(c.per_target[0].elements.len(), 2);
        assert_eq!(c.per_target[1].elements.len(), 1);
        assert!(c.per_target[2].elements.is_empty());
        assert_eq!(c.per_target[2].report.reasons, ["no-module"]);
        assert_eq!(fake.calls("ipc --id shell call woints list"), 1);
        assert!(c.timings.get("qs.list").is_some());
        // Cached: no rediscovery on the next request.
        p.collect_layers(&[&bar], budget(), &[]).await;
        assert_eq!(fake.calls("list --all"), 1);
        assert_eq!(fake.calls("ipc --pid"), 2);
        assert!(p.status().await.available);
    }

    #[tokio::test]
    async fn pid_change_rediscovers() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        p.collect_layers(&[&layer("quickshell:bar", 10)], budget(), &[])
            .await;
        *fake.instances.lock().unwrap() = vec![(20, "shell2", true)];
        let c = p
            .collect_layers(&[&layer("quickshell:bar", 20)], budget(), &[])
            .await;
        assert_eq!(fake.calls("list --all"), 2);
        assert_eq!(c.per_target[0].elements.len(), 2);
        assert_eq!(
            c.per_target[0].elements[0].handle.as_deref(),
            Some("qs:shell2:quickshell:bar/1")
        );
    }

    #[tokio::test]
    async fn foreign_pid_cached() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let mako = layer("notifications", 99);
        for _ in 0..3 {
            let c = p.collect_layers(&[&mako], budget(), &[]).await;
            assert!(c.per_target[0].elements.is_empty());
        }
        assert_eq!(fake.calls("list --all"), 1);
        assert_eq!(fake.calls("ipc --id"), 0);
    }

    #[tokio::test]
    async fn failed_list_forgets_instance() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        *fake.list_reply.lock().unwrap() = Err(RunError::Timeout);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let bar = layer("quickshell:bar", 10);
        let c = p.collect_layers(&[&bar], budget(), &[]).await;
        assert!(c.per_target[0].report.truncated);
        p.collect_layers(&[&bar], budget(), &[]).await;
        assert_eq!(fake.calls("list --all"), 2);
    }

    #[tokio::test]
    async fn invoke_results() {
        let fake = Fake::new(vec![]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let mut e = parse::elements_for(
            &layer("quickshell:bar", 10),
            "shell",
            &parse::parse_list(ITEMS).unwrap(),
        )
        .remove(0);
        assert_eq!(p.invoke(&e, Action::Click).await, Ok(()));
        assert_eq!(
            p.invoke(&e, Action::Right).await,
            Err("invoke returned \"unhandled\"".into())
        );
        e.handle = None;
        assert!(p.invoke(&e, Action::Click).await.is_err());
    }

    #[tokio::test]
    async fn no_layers_no_spawn() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let c = p.collect_layers(&[], budget(), &[]).await;
        assert!(c.per_target.is_empty());
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    fn ign(g: &[&str]) -> Vec<String> {
        g.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn ignored_instance_is_never_probed() {
        let fake = Fake::new(vec![(10, "shell", true), (11, "lock", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let ignore = ign(&["*/lock.qml"]);
        let bar = layer("quickshell:bar", 10);
        let lock = layer("quickshell:lock", 11);
        let c = p.collect_layers(&[&bar, &lock], budget(), &ignore).await;
        assert_eq!(c.per_target[0].elements.len(), 2);
        assert!(c.per_target[1].elements.is_empty());
        assert!(c.per_target[1].report.ignored());
        assert_eq!(fake.calls("ipc --pid 11"), 0);
        assert_eq!(fake.calls("ipc --id lock"), 0);
        let r = p.doctor(&ignore).await.unwrap();
        assert!(r.iter().find(|i| i.id == "lock").unwrap().ignored);
        let shell = r.iter().find(|i| i.id == "shell").unwrap();
        assert!(!shell.ignored);
        assert_eq!(shell.items, Ok(4));
        assert_eq!(fake.calls("ipc --pid 11"), 0);
        assert_eq!(fake.calls("ipc --id lock"), 0);
    }

    #[tokio::test]
    async fn reload_adds_glob() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let bar = layer("quickshell:bar", 10);
        p.collect_layers(&[&bar], budget(), &[]).await;
        assert_eq!(fake.calls("ipc --id shell call"), 1);
        let c = p
            .collect_layers(&[&bar], budget(), &ign(&["*/shell.qml"]))
            .await;
        assert!(c.per_target[0].report.ignored());
        assert!(c.per_target[0].elements.is_empty());
        assert_eq!(fake.calls("ipc --id shell call"), 1);
    }

    #[tokio::test]
    async fn reload_removes_glob() {
        let fake = Fake::new(vec![(10, "shell", true)]);
        let p = QuickshellIpcProvider::with_runner(&fake, Duration::from_millis(40));
        let bar = layer("quickshell:bar", 10);
        p.collect_layers(&[&bar], budget(), &ign(&["*/shell.qml"]))
            .await;
        assert_eq!(fake.calls("ipc --pid"), 0);
        let c = p.collect_layers(&[&bar], budget(), &[]).await;
        assert_eq!(fake.calls("ipc --pid"), 1);
        assert_eq!(c.per_target[0].elements.len(), 2);
    }

    fn live() -> bool {
        std::env::var("WOINTS_LIVE_TESTS").as_deref() == Ok("1")
    }

    /// Read-only: lists instances and, when the module is imported, its items.
    #[tokio::test]
    async fn live_doctor() {
        if !live() {
            return;
        }
        let p = QuickshellIpcProvider::new(Duration::from_millis(200));
        match p.doctor(&[]).await {
            Ok(r) => {
                for i in &r {
                    eprintln!(
                        "{} pid {} module {} items {:?}",
                        i.id, i.pid, i.module, i.items
                    );
                }
            }
            Err(e) => eprintln!("qs unavailable: {e}"),
        }
    }
}
