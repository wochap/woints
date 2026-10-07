//! Thin typed calls on the a11y bus. Every call pins its destination bus
//! name (design D14) and goes straight through `Connection::call_method`, so
//! no proxy caches or signal matches are set up per object.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use zbus::Connection;
use zvariant::{OwnedObjectPath, OwnedValue, Type};

pub const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
pub const APPLICATION: &str = "org.a11y.atspi.Application";
pub const COMPONENT: &str = "org.a11y.atspi.Component";
pub const ACTION: &str = "org.a11y.atspi.Action";
pub const COLLECTION: &str = "org.a11y.atspi.Collection";
pub const REGISTRY: &str = "org.a11y.atspi.Registry";
pub const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";
pub const NULL_PATH: &str = "/org/a11y/atspi/null";

/// `ATSPI_COORD_TYPE_WINDOW`; `SCREEN` is never used (design D16).
pub const COORD_WINDOW: u32 = 1;

/// An accessible object: owning bus name and object path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Obj {
    pub bus: String,
    pub path: OwnedObjectPath,
}

impl Obj {
    pub fn new(bus: &str, path: &str) -> Option<Self> {
        Some(Self {
            bus: bus.to_owned(),
            path: OwnedObjectPath::try_from(path).ok()?,
        })
    }

    pub fn root(bus: &str) -> Self {
        Self::new(bus, ROOT_PATH).expect("static path")
    }

    pub fn is_null(&self) -> bool {
        self.bus.is_empty() || self.path.as_str() == NULL_PATH
    }

    /// `bus:path`, stored in `Element.handle`.
    pub fn handle(&self) -> String {
        format!("{}:{}", self.bus, self.path.as_str())
    }

    pub fn from_handle(h: &str) -> Option<Self> {
        let (bus, path) = h.split_once(":/")?;
        Self::new(bus, &format!("/{path}"))
    }
}

impl fmt::Display for Obj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.handle())
    }
}

fn objs(v: Vec<(String, OwnedObjectPath)>) -> Vec<Obj> {
    v.into_iter()
        .map(|(bus, path)| Obj { bus, path })
        .filter(|o| !o.is_null())
        .collect()
}

pub async fn call<B, R>(
    c: &Connection,
    o: &Obj,
    iface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<R>
where
    B: Serialize + Type,
    R: DeserializeOwned + Type,
{
    let msg = c
        .call_method(
            Some(o.bus.as_str()),
            o.path.as_str(),
            Some(iface),
            method,
            body,
        )
        .await?;
    msg.body().deserialize::<R>()
}

pub async fn prop(c: &Connection, o: &Obj, iface: &str, name: &str) -> zbus::Result<OwnedValue> {
    call(
        c,
        o,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(iface, name),
    )
    .await
}

pub async fn prop_string(c: &Connection, o: &Obj, iface: &str, name: &str) -> zbus::Result<String> {
    let v = prop(c, o, iface, name).await?;
    String::try_from(v).map_err(zbus::Error::from)
}

pub async fn children(c: &Connection, o: &Obj) -> zbus::Result<Vec<Obj>> {
    call(c, o, ACCESSIBLE, "GetChildren", &()).await.map(objs)
}

pub async fn role_name(c: &Connection, o: &Obj) -> zbus::Result<String> {
    call(c, o, ACCESSIBLE, "GetRoleName", &()).await
}

/// Raw `GetState` words.
pub async fn state(c: &Connection, o: &Obj) -> zbus::Result<Vec<u32>> {
    call(c, o, ACCESSIBLE, "GetState", &()).await
}

pub async fn interfaces(c: &Connection, o: &Obj) -> zbus::Result<Vec<String>> {
    call(c, o, ACCESSIBLE, "GetInterfaces", &()).await
}

pub async fn name(c: &Connection, o: &Obj) -> zbus::Result<String> {
    prop_string(c, o, ACCESSIBLE, "Name").await
}

pub async fn parent(c: &Connection, o: &Obj) -> zbus::Result<Option<Obj>> {
    let v = prop(c, o, ACCESSIBLE, "Parent").await?;
    let r = atspi_common::ObjectRefOwned::try_from(v)
        .map_err(|e| zbus::Error::Failure(e.to_string()))?;
    Ok(r.name_as_str()
        .and_then(|n| Obj::new(n, r.path_as_str()))
        .filter(|o| !o.is_null()))
}

/// `Component.GetExtents(WINDOW)` as `(x, y, w, h)`.
pub async fn extents(c: &Connection, o: &Obj) -> zbus::Result<(i32, i32, i32, i32)> {
    call(c, o, COMPONENT, "GetExtents", &(COORD_WINDOW,)).await
}

pub async fn n_actions(c: &Connection, o: &Obj) -> zbus::Result<i32> {
    let v = prop(c, o, ACTION, "NActions").await?;
    i32::try_from(v).map_err(zbus::Error::from)
}

/// Action names, in index order.
pub async fn action_names(c: &Connection, o: &Obj) -> zbus::Result<Vec<String>> {
    let v: Vec<(String, String, String)> = call(c, o, ACTION, "GetActions", &()).await?;
    Ok(v.into_iter().map(|(n, _, _)| n).collect())
}

pub async fn do_action(c: &Connection, o: &Obj, index: i32) -> zbus::Result<bool> {
    call(c, o, ACTION, "DoAction", &(index,)).await
}

pub async fn grab_focus(c: &Connection, o: &Obj) -> zbus::Result<bool> {
    call(c, o, COMPONENT, "GrabFocus", &()).await
}

pub async fn get_matches(
    c: &Connection,
    o: &Obj,
    rule: &crate::rule::MatchRule,
) -> zbus::Result<Vec<Obj>> {
    let body = (rule, crate::rule::SORT_CANONICAL, 0i32, true);
    call(c, o, COLLECTION, "GetMatches", &body).await.map(objs)
}

/// Applications registered under the Registry root.
pub async fn registry_apps(c: &Connection) -> zbus::Result<Vec<Obj>> {
    let root = Obj {
        bus: REGISTRY.to_owned(),
        path: OwnedObjectPath::try_from(ROOT_PATH).expect("static path"),
    };
    children(c, &root).await
}

pub async fn pid_of(c: &Connection, bus: &str) -> zbus::Result<u32> {
    let msg = c
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetConnectionUnixProcessID",
            &(bus,),
        )
        .await?;
    msg.body().deserialize::<u32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_round_trip() {
        let o = Obj::new(":1.42", "/org/a11y/atspi/accessible/7").unwrap();
        assert_eq!(o.handle(), ":1.42:/org/a11y/atspi/accessible/7");
        assert_eq!(Obj::from_handle(&o.handle()), Some(o));
        assert!(Obj::from_handle("nope").is_none());
        assert!(Obj::new(":1.1", NULL_PATH).unwrap().is_null());
    }
}
