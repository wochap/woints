//! `AtspiActuator`: `DoAction` or `GrabFocus` (design D21).

use std::sync::Arc;

use woints_core::model::{ActionKind, Element};
use woints_core::ports::{ActError, Actuator};
use woints_core::profile::ToolkitProfile;

use crate::conn::A11yConnection;
use crate::dbus::{self, Obj};
use crate::provider::SharedApps;

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

pub fn plan(element: &[ActionKind], action: ActionKind) -> Plan {
    match action {
        ActionKind::Focus => Plan::GrabFocus,
        ActionKind::Activate => match element.first() {
            Some(ActionKind::Focus) => Plan::GrabFocus,
            Some(ActionKind::Activate) => Plan::DoAction,
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
}

impl Actuator for AtspiActuator {
    async fn act(&self, element: &Element, action: ActionKind) -> Result<(), ActError> {
        let p = plan(&element.actions, action);
        if p == Plan::NeedsPointer {
            return Err(ActError::NeedsPointer(action));
        }
        let obj = element
            .handle
            .as_deref()
            .and_then(Obj::from_handle)
            .ok_or(ActError::NeedsPointer(action))?;
        let (c, _) = self
            .conn
            .get()
            .await
            .map_err(|e| ActError::Failed(e.to_string()))?;
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
        if ok {
            Ok(())
        } else {
            Err(ActError::Failed(format!("{p:?} returned false")))
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
        use ActionKind::*;
        assert_eq!(plan(&[Activate], Activate), Plan::DoAction);
        assert_eq!(plan(&[Focus, Activate], Activate), Plan::GrabFocus);
        assert_eq!(plan(&[Activate], Focus), Plan::GrabFocus);
        assert_eq!(plan(&[Pointer], Activate), Plan::NeedsPointer);
        assert_eq!(plan(&[Activate], Right), Plan::NeedsPointer);
    }
}
