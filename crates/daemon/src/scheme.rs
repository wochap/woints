//! System colour scheme from the XDG settings portal (`org.freedesktop.appearance`
//! `color-scheme`), read at start and followed through `SettingChanged`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use woints_core::theme::{Scheme, SchemeSource};
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use crate::daemon::Daemon;

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Portal `color-scheme` value to a scheme; `0` and unknown values mean no
/// preference.
pub fn from_portal(value: u32) -> (Scheme, SchemeSource) {
    match value {
        1 => (Scheme::Dark, SchemeSource::Portal),
        2 => (Scheme::Light, SchemeSource::Portal),
        _ => (Scheme::Dark, SchemeSource::Fallback),
    }
}

/// Unwrap (possibly nested) variants down to a `u32`.
fn as_u32(v: &Value<'_>) -> Option<u32> {
    match v {
        Value::U32(n) => Some(*n),
        Value::Value(inner) => as_u32(inner),
        _ => None,
    }
}

async fn read(conn: &Connection) -> zbus::Result<u32> {
    let reply = conn
        .call_method(Some(DEST), PATH, Some(IFACE), "ReadOne", &(NAMESPACE, KEY))
        .await?;
    let v: OwnedValue = reply.body().deserialize()?;
    as_u32(&v).ok_or_else(|| zbus::Error::Failure("color-scheme is not a u32".into()))
}

async fn changes(conn: &Connection) -> zbus::Result<MessageStream> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(DEST)?
        .interface(IFACE)?
        .member("SettingChanged")?
        .path(PATH)?
        .build();
    MessageStream::for_match_rule(rule, conn, None).await
}

/// Follow the portal for the daemon's lifetime, retrying with backoff while
/// it is unreachable.
pub async fn watch(daemon: Arc<Daemon>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        match follow(&daemon).await {
            Ok(()) => backoff = BACKOFF_MIN,
            Err(e) => {
                tracing::debug!("colour scheme portal: {e}");
                daemon
                    .set_scheme(Scheme::Dark, SchemeSource::Fallback)
                    .await;
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

async fn follow(daemon: &Daemon) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    // Subscribe first so a change between the read and the stream is not lost.
    let mut stream = changes(&conn).await?;
    let (scheme, source) = from_portal(read(&conn).await?);
    daemon.set_scheme(scheme, source).await;
    tracing::info!(
        scheme = scheme.as_str(),
        source = source.as_str(),
        "colour scheme"
    );
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let Ok((ns, key, value)) = msg.body().deserialize::<(String, String, OwnedValue)>() else {
            continue;
        };
        if ns != NAMESPACE || key != KEY {
            continue;
        }
        let Some(n) = as_u32(&value) else { continue };
        let (scheme, source) = from_portal(n);
        daemon.set_scheme(scheme, source).await;
        tracing::info!(
            scheme = scheme.as_str(),
            source = source.as_str(),
            "colour scheme"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_values() {
        assert_eq!(from_portal(1), (Scheme::Dark, SchemeSource::Portal));
        assert_eq!(from_portal(2), (Scheme::Light, SchemeSource::Portal));
        assert_eq!(from_portal(0), (Scheme::Dark, SchemeSource::Fallback));
        assert_eq!(from_portal(9), (Scheme::Dark, SchemeSource::Fallback));
    }

    #[test]
    fn nested_variant() {
        let v = Value::Value(Box::new(Value::U32(2)));
        assert_eq!(as_u32(&v), Some(2));
    }
}
