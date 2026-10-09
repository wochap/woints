//! `AtspiActuator`: `DoAction`, `GrabFocus`, text and scrollbar reads (design D16, D21).

use std::sync::Arc;

use woints_core::model::{Action, AtspiCap, Element};
use woints_core::ports::{ActError, Actuator, ScrollValue};
use woints_core::profile::ToolkitProfile;

use crate::conn::A11yConnection;
use crate::dbus::{self, Obj};
use crate::provider::SharedApps;
use crate::rule::role;

pub struct AtspiActuator {
    conn: Arc<A11yConnection>,
    apps: SharedApps,
}

/// What the actuator will do for an element and requested action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    DoAction,
    GrabFocus,
    NeedsPointer,
}

/// `GrabFocus` for `focus` and for `click` on text entries, `DoAction` for
/// `click` on elements with actions.
pub fn plan(caps: &[AtspiCap], action: Action) -> Plan {
    match action {
        Action::Focus if !caps.is_empty() => Plan::GrabFocus,
        Action::Click => match caps.first() {
            Some(AtspiCap::GrabFocus) => Plan::GrabFocus,
            _ if caps.contains(&AtspiCap::DoAction) => Plan::DoAction,
            _ => Plan::NeedsPointer,
        },
        _ => Plan::NeedsPointer,
    }
}

/// Index of the action matching the earliest `preference` entry
/// (case-insensitive), else 0.
pub fn pick_action(names: &[String], preference: &[String]) -> i32 {
    preference
        .iter()
        .find_map(|p| names.iter().position(|n| p.eq_ignore_ascii_case(n)))
        .unwrap_or(0) as i32
}

impl AtspiActuator {
    pub fn new(conn: Arc<A11yConnection>, apps: SharedApps) -> Self {
        Self { conn, apps }
    }

    fn preference(&self, bus: &str) -> Vec<String> {
        let apps = self.apps.lock().unwrap_or_else(|e| e.into_inner());
        apps.profile_for(bus).unwrap_or_default().action_preference
    }

    async fn obj(&self, element: &Element) -> Result<(zbus::Connection, Obj), ActError> {
        let obj = element
            .handle
            .as_deref()
            .and_then(Obj::from_handle)
            .ok_or_else(|| ActError::Failed("element has no AT-SPI handle".into()))?;
        let (c, _) = self
            .conn
            .get()
            .await
            .map_err(|e| ActError::Failed(e.to_string()))?;
        Ok((c, obj))
    }

    /// `Text.GetText(0, -1)`, falling back to `Accessible.Name`.
    pub async fn text(&self, element: &Element) -> Result<String, ActError> {
        let (c, obj) = self.obj(element).await?;
        match dbus::text(&c, &obj).await {
            Ok(t) if !t.is_empty() => Ok(t),
            _ => dbus::name(&c, &obj)
                .await
                .map_err(|e| ActError::Failed(e.to_string())),
        }
    }

    /// `Value` of the container's scrollbar child, vertical first.
    pub async fn scroll_value(&self, element: &Element) -> Option<ScrollValue> {
        let (c, obj) = self.obj(element).await.ok()?;
        let mut bars = Vec::new();
        for ch in dbus::children(&c, &obj).await.ok()? {
            if dbus::role(&c, &ch).await.ok()? != role::SCROLL_BAR {
                continue;
            }
            let (_, _, w, h) = dbus::extents(&c, &ch).await.ok()?;
            bars.push((h >= w, ch));
        }
        bars.sort_by_key(|(vertical, _)| !vertical);
        let (_, bar) = bars.into_iter().next()?;
        let (current, min, max) = dbus::value(&c, &bar).await.ok()?;
        Some(ScrollValue { current, min, max })
    }
}

impl Actuator for AtspiActuator {
    /// `GrabFocus` returning `false` yields [`ActError::NeedsPointer`] so the
    /// caller falls back to a pointer click.
    async fn act(&self, element: &Element, action: Action) -> Result<(), ActError> {
        let p = plan(&element.caps, action);
        if p == Plan::NeedsPointer || element.handle.is_none() {
            return Err(ActError::NeedsPointer(action));
        }
        let (c, obj) = self.obj(element).await?;
        let ok = match p {
            Plan::GrabFocus => dbus::grab_focus(&c, &obj).await,
            Plan::DoAction => {
                let names = dbus::action_names(&c, &obj).await.unwrap_or_default();
                let i = pick_action(&names, &self.preference(&obj.bus));
                tracing::debug!(element = %obj, ?names, index = i, "DoAction");
                dbus::do_action(&c, &obj, i).await
            }
            Plan::NeedsPointer => unreachable!(),
        }
        .map_err(|e| ActError::Failed(e.to_string()))?;
        match (ok, p) {
            (true, _) => Ok(()),
            (false, Plan::GrabFocus) => Err(ActError::NeedsPointer(action)),
            (false, _) => Err(ActError::Failed(format!("{p:?} returned false"))),
        }
    }
}

impl crate::provider::AppCache {
    /// Profile of the cached app on `bus`, without a Hyprland class.
    pub fn profile_for(&self, bus: &str) -> Option<ToolkitProfile> {
        self.app(bus)
            .map(|a| ToolkitProfile::lookup(&a.toolkit, &a.version, ""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use woints_core::profile::DEFAULT_ACTION_PREFERENCE;

    fn pref() -> Vec<String> {
        DEFAULT_ACTION_PREFERENCE
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn preferred_action() {
        assert_eq!(
            pick_action(&names(&["press", "release", "click"]), &pref()),
            2
        );
        assert_eq!(pick_action(&names(&["release", "Press"]), &pref()), 1);
        assert_eq!(pick_action(&names(&["jump"]), &pref()), 0);
        assert_eq!(pick_action(&[], &pref()), 0);
    }

    #[test]
    fn plans() {
        use AtspiCap::*;
        assert_eq!(plan(&[DoAction], Action::Click), Plan::DoAction);
        assert_eq!(plan(&[GrabFocus, DoAction], Action::Click), Plan::GrabFocus);
        assert_eq!(plan(&[DoAction], Action::Focus), Plan::GrabFocus);
        assert_eq!(plan(&[Accessible], Action::Click), Plan::NeedsPointer);
        assert_eq!(plan(&[], Action::Focus), Plan::NeedsPointer);
        assert_eq!(plan(&[DoAction], Action::Right), Plan::NeedsPointer);
    }
}
