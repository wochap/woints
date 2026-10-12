//! Key capture through a Hyprland submap (popup-safe input).
//!
//! The compositor, not a focused layer surface, delivers hint keys: a
//! submap binds every label key to the `event` dispatcher, whose payloads
//! come back on socket2 as `custom>>` lines. Nothing here removes binds
//! (`Keybind:remove()` crashes Hyprland 0.56.2); a changed table gets a new
//! versioned submap name instead.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use woints_core::ports::{KeyEvent, KeyPress, KeySource, Modifiers};

use crate::{DispatchSyntax, Error, Event, Hypr, Result};

const MOD_SHIFT: u32 = 1;
const MOD_CTRL: u32 = 2;
const MOD_ALT: u32 = 4;
const MOD_LOGO: u32 = 8;

/// Hyprland modifier names and the mask the payload carries for each.
const MODS: [(&str, u32); 5] = [
    ("", 0),
    ("SHIFT", MOD_SHIFT),
    ("CTRL", MOD_CTRL),
    ("ALT", MOD_ALT),
    ("SUPER", MOD_LOGO),
];

/// Modifier keysyms bound to nothing, so pressing one doesn't hit the catchall.
const MODIFIER_KEYS: [&str; 8] = [
    "Shift_L",
    "Shift_R",
    "Control_L",
    "Control_R",
    "Alt_L",
    "Alt_R",
    "Super_L",
    "Super_R",
];

/// One captured key: the keysym name binds use and the keycode the payload
/// reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindKey {
    pub keysym: String,
    pub keycode: u32,
}

/// The binds of one submap, named after their contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmapTable {
    prefix: String,
    name: String,
    keys: BTreeMap<String, u32>,
}

impl SubmapTable {
    /// `<prefix>-<8 hex of a hash of the keys>`.
    pub fn new(prefix: &str, keys: &[BindKey]) -> Self {
        let keys: BTreeMap<String, u32> =
            keys.iter().map(|k| (k.keysym.clone(), k.keycode)).collect();
        let mut text = String::from(prefix);
        for (sym, code) in &keys {
            text.push_str(&format!("|{sym}:{code}"));
        }
        // FNV-1a: stable across runs and builds, unlike `DefaultHasher`.
        let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
        Self {
            name: format!("{prefix}-{:08x}", hash as u32),
            prefix: prefix.to_owned(),
            keys,
        }
    }

    /// The versioned submap name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `input.submap`, the name without the version.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    fn key_payload(&self, keycode: u32, mask: u32) -> String {
        format!("{} key {keycode} {mask}", self.name)
    }

    fn escape_payload(&self) -> String {
        format!("{} escape", self.name)
    }

    /// Lua `eval` script: only `define_submap`, `bind`, `dispatch` and `event`.
    pub fn lua_script(&self) -> String {
        let mut s = format!("hl.define_submap(\"{}\", function()\n", self.name);
        for (sym, &code) in &self.keys {
            for (mods, mask) in MODS {
                let key = if mods.is_empty() {
                    sym.clone()
                } else {
                    format!("{mods} + {sym}")
                };
                s.push_str(&format!(
                    "hl.bind(\"{key}\", hl.dsp.event(\"{}\"))\n",
                    self.key_payload(code, mask)
                ));
            }
        }
        for sym in MODIFIER_KEYS {
            for (mods, _) in MODS {
                let key = if mods.is_empty() {
                    sym.to_owned()
                } else {
                    format!("{mods} + {sym}")
                };
                s.push_str(&format!("hl.bind(\"{key}\", hl.dsp.no_op())\n"));
            }
        }
        s.push_str(&format!(
            "hl.bind(\"Escape\", function()\nhl.dispatch(hl.dsp.event(\"{}\"))\nhl.dispatch(hl.dsp.submap(\"reset\"))\nend)\n",
            self.escape_payload()
        ));
        s.push_str("hl.bind(\"catchall\", hl.dsp.submap(\"reset\"))\nend)");
        s
    }

    /// Legacy `[[BATCH]]` request of `keyword` commands.
    pub fn legacy_batch(&self) -> String {
        let mut cmds = vec![format!("keyword submap {}", self.name)];
        let mut bind = |mods: &str, key: &str, dispatcher: &str, arg: &str| {
            cmds.push(format!("keyword bind {mods},{key},{dispatcher},{arg}"));
        };
        for (sym, &code) in &self.keys {
            for (mods, mask) in MODS {
                bind(mods, sym, "event", &self.key_payload(code, mask));
            }
        }
        for sym in MODIFIER_KEYS {
            for (mods, _) in MODS {
                bind(mods, sym, "event", &format!("{} mod", self.name));
            }
        }
        bind("", "Escape", "event", &self.escape_payload());
        bind("", "Escape", "submap", "reset");
        bind("", "catchall", "submap", "reset");
        cmds.push("keyword submap reset".into());
        format!("[[BATCH]]{}", cmds.join(" ; "))
    }
}

/// Lua script that arms the compositor-side reset for `token`.
pub fn timer_script(name: &str, token: u64, ms: u32) -> String {
    format!(
        "woints_keys = woints_keys or {{}}\n\
         woints_keys.token = {token}\n\
         hl.timer(function()\n\
         if woints_keys.token == {token} and hl.get_current_submap() == \"{name}\" then\n\
         hl.dispatch(hl.dsp.submap(\"reset\"))\n\
         end\n\
         end, {{ timeout = {ms}, type = \"oneshot\" }})"
    )
}

const CLEAR_TOKEN_SCRIPT: &str = "woints_keys = woints_keys or {}\nwoints_keys.token = nil";

#[derive(Debug, Deserialize)]
struct BindRow {
    #[serde(default)]
    submap: String,
}

/// Whether `j/binds` output lists a bind in submap `name`.
fn has_binds(json: &str, name: &str) -> bool {
    serde_json::from_str::<Vec<BindRow>>(json)
        .map(|rows| rows.iter().any(|r| r.submap == name))
        .unwrap_or(false)
}

/// The submap named by a `j/submap` reply; `None` for the default one.
fn parse_submap(reply: &str) -> Option<String> {
    let reply = reply.trim();
    let name = match serde_json::from_str::<serde_json::Value>(reply) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(serde_json::Value::Object(o)) => o
            .get("submap")
            .or_else(|| o.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned(),
        _ => reply.to_owned(),
    };
    (!name.is_empty() && name != "default").then_some(name)
}

impl Hypr {
    /// The submap Hyprland is in, `None` for the default one.
    pub async fn current_submap(&self) -> Result<Option<String>> {
        Ok(parse_submap(&self.request("j/submap").await?))
    }

    pub async fn submap_enter(&self, name: &str) -> Result<()> {
        match self.dispatch_syntax().await {
            DispatchSyntax::Lua => self.dispatch(&format!("hl.dsp.submap(\"{name}\")")).await,
            DispatchSyntax::Legacy => self.dispatch(&format!("submap {name}")).await,
        }
    }

    pub async fn submap_reset(&self) -> Result<()> {
        self.submap_enter("reset").await
    }

    async fn eval(&self, script: &str) -> Result<()> {
        let reply = self.request(&format!("eval {script}")).await?;
        if reply.trim() == "ok" {
            Ok(())
        } else {
            Err(Error::Dispatch(reply.trim().to_owned()))
        }
    }

    /// Define the submap unless `j/binds` already lists it. Never removes.
    pub async fn install_submap(&self, table: &SubmapTable) -> Result<()> {
        let binds = self.request("j/binds").await?;
        if has_binds(&binds, table.name()) {
            return Ok(());
        }
        match self.dispatch_syntax().await {
            DispatchSyntax::Lua => self.eval(&table.lua_script()).await,
            DispatchSyntax::Legacy => {
                let reply = self.request(&table.legacy_batch()).await?;
                if reply.replace("ok", "").trim().is_empty() {
                    Ok(())
                } else {
                    Err(Error::Dispatch(reply.trim().to_owned()))
                }
            }
        }
    }

    /// Reset the submap after `ms` unless `clear_submap_token` ran or a new
    /// token was armed. Lua builds only.
    pub async fn arm_submap_timer(&self, name: &str, token: u64, ms: u32) -> Result<()> {
        match self.dispatch_syntax().await {
            DispatchSyntax::Lua => self.eval(&timer_script(name, token, ms)).await,
            DispatchSyntax::Legacy => Err(Error::Unsupported("submap timer")),
        }
    }

    pub async fn clear_submap_token(&self) -> Result<()> {
        match self.dispatch_syntax().await {
            DispatchSyntax::Lua => self.eval(CLEAR_TOKEN_SCRIPT).await,
            DispatchSyntax::Legacy => Err(Error::Unsupported("submap timer")),
        }
    }
}

/// Builds a [`KeyPress`] (keysym, utf8) from a keycode and modifiers.
pub type Translate = Arc<dyn Fn(u32, Modifiers) -> Option<KeyPress> + Send + Sync>;

#[derive(Debug, Default)]
struct State {
    /// This side believes Hyprland is in the submap.
    entered: bool,
    /// Resets requested by `end` whose `submap>>` event hasn't arrived.
    own_resets: u32,
    /// An `escape` payload arrived; the next reset is the Escape key's.
    escape: bool,
}

/// [`KeySource`] over a Hyprland submap.
pub struct SubmapKeys {
    hypr: Hypr,
    table: SubmapTable,
    translate: Translate,
    timer_ms: u32,
    state: Mutex<State>,
}

impl SubmapKeys {
    /// Install `table`, and reset a submap of this family left behind by a
    /// previous daemon.
    pub async fn install(
        hypr: Hypr,
        table: SubmapTable,
        translate: Translate,
        timer_ms: u32,
    ) -> Result<Self> {
        hypr.install_submap(&table).await?;
        let family = format!("{}-", table.prefix());
        if let Some(cur) = hypr.current_submap().await?
            && cur.starts_with(&family)
        {
            tracing::info!(submap = %cur, "resetting a stale woints submap");
            hypr.submap_reset().await?;
        }
        Ok(Self {
            hypr,
            table,
            translate,
            timer_ms,
            state: Mutex::new(State::default()),
        })
    }

    pub fn name(&self) -> &str {
        self.table.name()
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Feed one socket2 event; `Some` when it is session input.
    pub fn on_event(&self, ev: &Event) -> Option<KeyEvent> {
        match ev {
            Event::Custom { payload } => {
                let rest = payload.strip_prefix(self.table.name())?.strip_prefix(' ')?;
                let mut st = self.st();
                if !st.entered {
                    return None;
                }
                let mut parts = rest.split(' ');
                match parts.next()? {
                    "escape" => {
                        st.escape = true;
                        None
                    }
                    "key" => {
                        drop(st);
                        let keycode = parts.next()?.parse().ok()?;
                        let mask: u32 = parts.next()?.parse().ok()?;
                        let mods = Modifiers {
                            shift: mask & MOD_SHIFT != 0,
                            ctrl: mask & MOD_CTRL != 0,
                            alt: mask & MOD_ALT != 0,
                            logo: mask & MOD_LOGO != 0,
                        };
                        (self.translate)(keycode, mods).map(KeyEvent::Key)
                    }
                    _ => None,
                }
            }
            Event::Submap { name } if name == self.table.name() => None,
            Event::Submap { .. } => {
                let mut st = self.st();
                if st.own_resets > 0 {
                    st.own_resets -= 1;
                    return None;
                }
                if !st.entered {
                    return None;
                }
                st.entered = false;
                if std::mem::take(&mut st.escape) {
                    Some(KeyEvent::Escape)
                } else {
                    Some(KeyEvent::Ended("submap left".into()))
                }
            }
            _ => None,
        }
    }
}

impl KeySource for SubmapKeys {
    fn kind(&self) -> String {
        format!("submap {}", self.table.name())
    }

    async fn begin(&self, token: u64) {
        let enter = {
            let mut st = self.st();
            st.escape = false;
            !std::mem::replace(&mut st.entered, true)
        };
        if enter && let Err(e) = self.hypr.submap_enter(self.table.name()).await {
            tracing::warn!("entering submap: {e}");
        }
        match self
            .hypr
            .arm_submap_timer(self.table.name(), token, self.timer_ms)
            .await
        {
            Ok(()) | Err(Error::Unsupported(_)) => {}
            Err(e) => tracing::warn!("submap timer: {e}"),
        }
    }

    async fn end(&self) {
        let was_entered = {
            let mut st = self.st();
            st.escape = false;
            let was = std::mem::replace(&mut st.entered, false);
            if was {
                st.own_resets += 1;
            }
            was
        };
        if !was_entered {
            return;
        }
        match self.hypr.clear_submap_token().await {
            Ok(()) | Err(Error::Unsupported(_)) => {}
            Err(e) => tracing::warn!("clearing submap token: {e}"),
        }
        if let Err(e) = self.hypr.submap_reset().await {
            tracing::warn!("resetting submap: {e}");
            let mut st = self.st();
            st.own_resets = st.own_resets.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn keys() -> Vec<BindKey> {
        ["a", "s", "semicolon"]
            .iter()
            .zip([38, 39, 47])
            .map(|(s, c)| BindKey {
                keysym: (*s).into(),
                keycode: c,
            })
            .collect()
    }

    fn table() -> SubmapTable {
        SubmapTable::new("woints-keys", &keys())
    }

    #[test]
    fn name_is_versioned_by_table() {
        let t = table();
        let (prefix, hash) = t.name().rsplit_once('-').unwrap();
        assert_eq!(prefix, "woints-keys");
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(t, table());
        let mut other = keys();
        other[0].keysym = "b".into();
        assert_ne!(SubmapTable::new("woints-keys", &other).name(), t.name());
        assert_ne!(SubmapTable::new("mine", &keys()).name(), t.name());
    }

    #[test]
    fn lua_script_shape() {
        let t = SubmapTable::new(
            "k",
            &[BindKey {
                keysym: "a".into(),
                keycode: 38,
            }],
        );
        let s = t.lua_script();
        let n = t.name();
        assert!(s.starts_with(&format!("hl.define_submap(\"{n}\", function()\n")));
        assert!(s.contains(&format!("hl.bind(\"a\", hl.dsp.event(\"{n} key 38 0\"))\n")));
        assert!(s.contains(&format!(
            "hl.bind(\"SHIFT + a\", hl.dsp.event(\"{n} key 38 1\"))\n"
        )));
        assert!(s.contains(&format!(
            "hl.bind(\"SUPER + a\", hl.dsp.event(\"{n} key 38 8\"))\n"
        )));
        assert!(s.contains("hl.bind(\"Shift_L\", hl.dsp.no_op())\n"));
        assert!(s.contains(&format!(
            "hl.bind(\"Escape\", function()\nhl.dispatch(hl.dsp.event(\"{n} escape\"))\nhl.dispatch(hl.dsp.submap(\"reset\"))\nend)\n"
        )));
        assert!(s.ends_with("hl.bind(\"catchall\", hl.dsp.submap(\"reset\"))\nend)"));
        for banned in ["remove", "unbind"] {
            assert!(!s.contains(banned));
        }
    }

    #[test]
    fn legacy_batch_shape() {
        let t = SubmapTable::new(
            "k",
            &[BindKey {
                keysym: "a".into(),
                keycode: 38,
            }],
        );
        let b = t.legacy_batch();
        let n = t.name();
        assert!(b.starts_with(&format!("[[BATCH]]keyword submap {n} ; ")));
        assert!(b.contains(&format!("keyword bind ,a,event,{n} key 38 0 ; ")));
        assert!(b.contains(&format!("keyword bind SHIFT,a,event,{n} key 38 1 ; ")));
        let esc = b
            .find(&format!("keyword bind ,Escape,event,{n} escape"))
            .unwrap();
        let reset = b.find("keyword bind ,Escape,submap,reset").unwrap();
        assert!(esc < reset);
        assert!(b.ends_with("keyword bind ,catchall,submap,reset ; keyword submap reset"));
        assert!(!b.contains("unbind"));
    }

    #[test]
    fn timer_script_shape() {
        let s = timer_script("k-1", 7, 17000);
        assert!(s.contains("woints_keys.token = 7\n"));
        assert!(s.contains("woints_keys.token == 7 and hl.get_current_submap() == \"k-1\""));
        assert!(s.contains("timeout = 17000"));
        assert!(!s.contains("remove"));
    }

    #[test]
    fn submap_replies() {
        assert_eq!(parse_submap("woints"), Some("woints".into()));
        assert_eq!(parse_submap("\"woints\"\n"), Some("woints".into()));
        assert_eq!(parse_submap("{\"submap\": \"w\"}"), Some("w".into()));
        assert_eq!(parse_submap(""), None);
        assert_eq!(parse_submap("default"), None);
        assert_eq!(parse_submap("{\"submap\": \"\"}"), None);
    }

    /// Serve one connection per reply, recording the requests.
    async fn serve(
        tag: &str,
        replies: Vec<&'static str>,
    ) -> (PathBuf, tokio::task::JoinHandle<Vec<String>>) {
        let dir =
            std::env::temp_dir().join(format!("woints-hypr-submap-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let l = tokio::net::UnixListener::bind(dir.join(".socket.sock")).unwrap();
        let srv = tokio::spawn(async move {
            let mut seen = Vec::new();
            for reply in replies {
                let (mut s, _) = l.accept().await.unwrap();
                let mut buf = vec![0u8; 1 << 20];
                let n = s.read(&mut buf).await.unwrap();
                seen.push(String::from_utf8_lossy(&buf[..n]).into_owned());
                s.write_all(reply.as_bytes()).await.unwrap();
            }
            seen
        });
        (dir, srv)
    }

    #[tokio::test]
    async fn install_defines_a_missing_submap_over_lua() {
        let (dir, srv) = serve("lua-new", vec!["[]", "ok"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        let t = table();
        h.install_submap(&t).await.unwrap();
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen[0], "j/binds");
        assert_eq!(seen[1], format!("eval {}", t.lua_script()));
    }

    #[tokio::test]
    async fn install_reports_hyprland_errors() {
        let (dir, srv) = serve("lua-err", vec!["[]", "lua error: boom"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        let r = h.install_submap(&table()).await;
        srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        match r {
            Err(Error::Dispatch(m)) => assert_eq!(m, "lua error: boom"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn install_sends_the_batch_on_legacy() {
        let (dir, srv) = serve("legacy-new", vec!["[]", "okokok"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Legacy);
        let t = table();
        h.install_submap(&t).await.unwrap();
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen[1], t.legacy_batch());
    }

    #[tokio::test]
    async fn install_keeps_an_existing_submap() {
        let t = table();
        let binds: &'static str =
            Box::leak(format!("[{{\"submap\":\"{}\",\"key\":\"a\"}}]", t.name()).into_boxed_str());
        let (dir, srv) = serve("existing", vec![binds]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        h.install_submap(&t).await.unwrap();
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen, ["j/binds"]);
    }

    #[tokio::test]
    async fn timer_is_unsupported_on_legacy() {
        let h = Hypr::with_dir("/nonexistent").with_syntax(DispatchSyntax::Legacy);
        assert!(matches!(
            h.arm_submap_timer("k", 1, 10).await,
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            h.clear_submap_token().await,
            Err(Error::Unsupported(_))
        ));
    }

    fn keys_source() -> SubmapKeys {
        let translate: Translate = Arc::new(|keycode, mods| {
            Some(KeyPress {
                keycode,
                keysym: u32::from(b'a'),
                utf8: Some("a".into()),
                mods,
            })
        });
        let t = table();
        let k = SubmapKeys {
            hypr: Hypr::with_dir("/nonexistent"),
            table: t,
            translate,
            timer_ms: 17000,
            state: Mutex::new(State::default()),
        };
        k.st().entered = true;
        k
    }

    fn custom(k: &SubmapKeys, rest: &str) -> Event {
        Event::Custom {
            payload: format!("{} {rest}", k.name()),
        }
    }

    fn left(name: &str) -> Event {
        Event::Submap { name: name.into() }
    }

    #[test]
    fn payload_becomes_a_key_press() {
        let k = keys_source();
        match k.on_event(&custom(&k, "key 38 3")) {
            Some(KeyEvent::Key(p)) => {
                assert_eq!(p.keycode, 38);
                assert!(p.mods.shift && p.mods.ctrl && !p.mods.alt && !p.mods.logo);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            k.on_event(&Event::Custom {
                payload: "other key 38 0".into()
            }),
            None
        );
    }

    #[test]
    fn own_reset_is_ignored() {
        let k = keys_source();
        {
            let mut st = k.st();
            st.entered = false;
            st.own_resets = 1;
        }
        assert_eq!(k.on_event(&left("")), None);
        assert_eq!(k.st().own_resets, 0);
        assert_eq!(k.on_event(&left("")), None);
    }

    #[test]
    fn escape_then_reset_is_escape() {
        let k = keys_source();
        assert_eq!(k.on_event(&custom(&k, "escape")), None);
        assert_eq!(k.on_event(&left("")), Some(KeyEvent::Escape));
        assert_eq!(k.on_event(&left("")), None);
    }

    #[test]
    fn bare_reset_is_ended() {
        let k = keys_source();
        assert_eq!(
            k.on_event(&left("")),
            Some(KeyEvent::Ended("submap left".into()))
        );
        let k = keys_source();
        assert_eq!(
            k.on_event(&left("woints")),
            Some(KeyEvent::Ended("submap left".into()))
        );
    }

    #[test]
    fn entering_our_submap_is_not_input() {
        let k = keys_source();
        assert_eq!(k.on_event(&left(k.name())), None);
        assert!(k.st().entered);
    }
}
