//! Key capture through a Hyprland submap (popup-safe input).
//!
//! The submap itself lives in the woints Hyprland Lua module
//! (`integrations/hyprland/woints.lua`), which reports key presses and
//! releases as `custom>>` lines on socket2. Nothing here defines, changes or
//! removes binds, submaps or timers: the daemon only enters and resets the
//! submap and reads `j/binds` and `j/submap`.

use std::sync::Mutex;

use serde::Deserialize;

use woints_core::ports::{KeyEvent, KeySource};

use crate::{DispatchSyntax, Error, Event, Hypr, Result};

/// Why [`SubmapKeys::install`] fails when `j/binds` lists no bind in the submap.
pub const MODULE_MISSING: &str = "Hyprland module not loaded";

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

    /// Whether `j/binds` lists any bind in submap `name`.
    pub async fn has_submap_binds(&self, name: &str) -> Result<bool> {
        Ok(has_binds(&self.request("j/binds").await?, name))
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
}

/// What a socket2 event means for the submap source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmapInput {
    /// A raw key report from the module; the daemon's xkb state translates it.
    Key { keycode: u32, pressed: bool },
    /// A session-level event.
    Session(KeyEvent),
}

#[derive(Debug, Default)]
struct State {
    /// This side believes Hyprland is in the submap.
    entered: bool,
    /// Resets requested by `end` whose `submap>>` event hasn't arrived.
    own_resets: u32,
    /// An `escape` payload arrived; the next reset is the Escape key's.
    escape: bool,
}

/// [`KeySource`] over the module's Hyprland submap.
pub struct SubmapKeys {
    hypr: Hypr,
    name: String,
    state: Mutex<State>,
}

impl SubmapKeys {
    /// Check that the module's submap `name` exists on a Lua build, and reset
    /// the submap if a previous daemon left Hyprland in it.
    pub async fn install(hypr: Hypr, name: &str) -> Result<Self> {
        if hypr.dispatch_syntax().await != DispatchSyntax::Lua {
            return Err(Error::Unsupported("not a Lua build"));
        }
        if !hypr.has_submap_binds(name).await? {
            return Err(Error::Unsupported(MODULE_MISSING));
        }
        if hypr.current_submap().await?.as_deref() == Some(name) {
            tracing::info!(submap = %name, "resetting a stale woints submap");
            hypr.submap_reset().await?;
        }
        Ok(Self {
            hypr,
            name: name.to_owned(),
            state: Mutex::new(State::default()),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Feed one socket2 event; `Some` when it is session input.
    pub fn on_event(&self, ev: &Event) -> Option<SubmapInput> {
        match ev {
            Event::Custom { payload } => {
                let rest = payload
                    .strip_prefix(self.name.as_str())?
                    .strip_prefix(' ')?;
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
                        let keycode = parts.next()?.parse().ok()?;
                        let pressed = match parts.next()? {
                            "1" => true,
                            "0" => false,
                            _ => return None,
                        };
                        Some(SubmapInput::Key { keycode, pressed })
                    }
                    _ => None,
                }
            }
            Event::Submap { name } if *name == self.name => None,
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
                    Some(SubmapInput::Session(KeyEvent::Escape))
                } else {
                    Some(SubmapInput::Session(KeyEvent::Ended("submap left".into())))
                }
            }
            _ => None,
        }
    }
}

impl KeySource for SubmapKeys {
    fn kind(&self) -> String {
        format!("submap {}", self.name)
    }

    async fn begin(&self, _token: u64) {
        let enter = {
            let mut st = self.st();
            st.escape = false;
            !std::mem::replace(&mut st.entered, true)
        };
        if enter && let Err(e) = self.hypr.submap_enter(&self.name).await {
            tracing::warn!("entering submap: {e}");
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

    const BINDS: &str = "[{\"submap\":\"woints-keys\",\"key\":\"\",\"catchall\":true}]";

    #[tokio::test]
    async fn submap_binds_present_and_absent() {
        let (dir, srv) = serve("binds", vec![BINDS, BINDS]).await;
        let h = Hypr::with_dir(&dir);
        assert!(h.has_submap_binds("woints-keys").await.unwrap());
        assert!(!h.has_submap_binds("other").await.unwrap());
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen, ["j/binds", "j/binds"]);
    }

    #[tokio::test]
    async fn current_submap_is_parsed() {
        let (dir, srv) = serve("current", vec!["woints-keys", ""]).await;
        let h = Hypr::with_dir(&dir);
        assert_eq!(
            h.current_submap().await.unwrap().as_deref(),
            Some("woints-keys")
        );
        assert_eq!(h.current_submap().await.unwrap(), None);
        srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_sends_no_definition() {
        let (dir, srv) = serve("install", vec![BINDS, "default"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        let k = SubmapKeys::install(h, "woints-keys").await.unwrap();
        assert_eq!(k.kind(), "submap woints-keys");
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(seen, ["j/binds", "j/submap"]);
    }

    #[tokio::test]
    async fn install_resets_a_stale_submap() {
        let (dir, srv) = serve("stale", vec![BINDS, "woints-keys", "ok"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        SubmapKeys::install(h, "woints-keys").await.unwrap();
        let seen = srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(seen[2].contains("hl.dsp.submap(\"reset\")"));
    }

    #[tokio::test]
    async fn install_without_the_module() {
        let (dir, srv) = serve("nomodule", vec!["[]"]).await;
        let h = Hypr::with_dir(&dir).with_syntax(DispatchSyntax::Lua);
        let r = SubmapKeys::install(h, "woints-keys").await;
        srv.await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(
            r,
            Err(Error::Unsupported("Hyprland module not loaded"))
        ));
    }

    #[tokio::test]
    async fn install_on_a_legacy_build() {
        let h = Hypr::with_dir("/nonexistent").with_syntax(DispatchSyntax::Legacy);
        assert!(matches!(
            SubmapKeys::install(h, "woints-keys").await,
            Err(Error::Unsupported("not a Lua build"))
        ));
    }

    fn keys_source() -> SubmapKeys {
        let k = SubmapKeys {
            hypr: Hypr::with_dir("/nonexistent"),
            name: "woints-keys".into(),
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
    fn payloads_become_raw_keys() {
        let k = keys_source();
        assert_eq!(
            k.on_event(&custom(&k, "key 38 1")),
            Some(SubmapInput::Key {
                keycode: 38,
                pressed: true
            })
        );
        assert_eq!(
            k.on_event(&custom(&k, "key 38 0")),
            Some(SubmapInput::Key {
                keycode: 38,
                pressed: false
            })
        );
    }

    #[test]
    fn unknown_state_and_foreign_names_are_ignored() {
        let k = keys_source();
        assert_eq!(k.on_event(&custom(&k, "key 38 2")), None);
        assert_eq!(k.on_event(&custom(&k, "key x 1")), None);
        assert_eq!(
            k.on_event(&Event::Custom {
                payload: "other key 38 1".into()
            }),
            None
        );
        assert_eq!(
            k.on_event(&Event::Custom {
                payload: "woints-keys-2 key 38 1".into()
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
        assert_eq!(
            k.on_event(&left("")),
            Some(SubmapInput::Session(KeyEvent::Escape))
        );
        assert_eq!(k.on_event(&left("")), None);
    }

    #[test]
    fn bare_reset_is_ended() {
        let ended = Some(SubmapInput::Session(KeyEvent::Ended("submap left".into())));
        let k = keys_source();
        assert_eq!(k.on_event(&left("")), ended);
        let k = keys_source();
        assert_eq!(k.on_event(&left("woints")), ended);
    }

    #[test]
    fn entering_our_submap_is_not_input() {
        let k = keys_source();
        assert_eq!(k.on_event(&left(k.name())), None);
        assert!(k.st().entered);
    }
}
