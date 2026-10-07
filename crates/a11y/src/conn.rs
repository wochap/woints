//! Persistent connection to the accessibility bus (design D14).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use zbus::Connection;
use zbus::proxy::CacheProperties;

use woints_core::ports::ProviderError;

const BACKOFF_MIN: Duration = Duration::from_millis(100);
const BACKOFF_MAX: Duration = Duration::from_secs(5);

struct Inner {
    conn: Option<Connection>,
    /// Bumped on every new connection so caches keyed to a connection reset.
    generation: u64,
    backoff: Duration,
    retry_at: Option<Instant>,
    last_error: Option<String>,
}

/// One zbus connection to the a11y bus for the daemon's lifetime. Address
/// comes from `org.a11y.Bus.GetAddress`; never forks `busctl`.
pub struct A11yConnection {
    inner: Mutex<Inner>,
    connecting: tokio::sync::Mutex<()>,
}

impl Default for A11yConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl A11yConnection {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                conn: None,
                generation: 0,
                backoff: BACKOFF_MIN,
                retry_at: None,
                last_error: None,
            }),
            connecting: tokio::sync::Mutex::new(()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The open connection and its generation, connecting if needed. While
    /// in backoff after a failure this returns `Unavailable` without trying.
    pub async fn get(&self) -> Result<(Connection, u64), ProviderError> {
        if let Some(c) = self.cached()? {
            return Ok(c);
        }
        let _guard = self.connecting.lock().await;
        if let Some(c) = self.cached()? {
            return Ok(c);
        }
        match open().await {
            Ok(conn) => {
                let mut i = self.lock();
                i.generation += 1;
                i.conn = Some(conn.clone());
                i.backoff = BACKOFF_MIN;
                i.retry_at = None;
                i.last_error = None;
                tracing::info!(generation = i.generation, "a11y bus connected");
                Ok((conn, i.generation))
            }
            Err(e) => {
                let mut i = self.lock();
                i.retry_at = Some(Instant::now() + i.backoff);
                i.backoff = (i.backoff * 2).min(BACKOFF_MAX);
                i.last_error = Some(e.clone());
                tracing::warn!("a11y bus unavailable: {e}");
                Err(ProviderError::Unavailable(e))
            }
        }
    }

    fn cached(&self) -> Result<Option<(Connection, u64)>, ProviderError> {
        let i = self.lock();
        if let Some(c) = &i.conn {
            return Ok(Some((c.clone(), i.generation)));
        }
        if i.retry_at.is_some_and(|t| Instant::now() < t) {
            return Err(ProviderError::Unavailable(
                i.last_error.clone().unwrap_or_else(|| "a11y bus".into()),
            ));
        }
        Ok(None)
    }

    /// Drop the connection after a transport error; the next `get` reconnects.
    pub fn invalidate(&self, generation: u64, why: &str) {
        let mut i = self.lock();
        if i.generation == generation && i.conn.take().is_some() {
            tracing::warn!("a11y bus connection dropped: {why}");
            i.last_error = Some(why.to_owned());
        }
    }

    /// Whether the last attempt succeeded (or none was made yet) and the last error.
    pub fn status(&self) -> (bool, Option<String>) {
        let i = self.lock();
        (
            i.conn.is_some() || i.retry_at.is_none(),
            i.last_error.clone(),
        )
    }
}

/// `org.a11y.Bus` facts for `doctor`, read on a fresh session connection.
#[derive(Debug, Clone, PartialEq)]
pub struct BusInfo {
    pub address: Result<String, String>,
    /// `org.a11y.Status.IsEnabled`.
    pub is_enabled: Option<bool>,
}

pub async fn bus_info() -> BusInfo {
    let session = match Connection::session().await {
        Ok(s) => s,
        Err(e) => {
            return BusInfo {
                address: Err(format!("session bus: {e}")),
                is_enabled: None,
            };
        }
    };
    let address = match atspi_proxies::bus::BusProxy::builder(&session)
        .cache_properties(CacheProperties::No)
        .build()
        .await
    {
        Ok(b) => b
            .get_address()
            .await
            .map_err(|e| format!("org.a11y.Bus.GetAddress: {e}")),
        Err(e) => Err(format!("org.a11y.Bus: {e}")),
    };
    let is_enabled = match atspi_proxies::bus::StatusProxy::builder(&session)
        .cache_properties(CacheProperties::No)
        .build()
        .await
    {
        Ok(s) => s.is_enabled().await.ok(),
        Err(_) => None,
    };
    BusInfo {
        address,
        is_enabled,
    }
}

async fn open() -> Result<Connection, String> {
    let session = Connection::session()
        .await
        .map_err(|e| format!("session bus: {e}"))?;
    let bus = atspi_proxies::bus::BusProxy::builder(&session)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|e| format!("org.a11y.Bus: {e}"))?;
    let address = bus
        .get_address()
        .await
        .map_err(|e| format!("org.a11y.Bus.GetAddress: {e}"))?;
    zbus::connection::Builder::address(address.as_str())
        .map_err(|e| format!("a11y bus address {address}: {e}"))?
        .build()
        .await
        .map_err(|e| format!("a11y bus {address}: {e}"))
}

/// Whether a zbus error means the connection itself is gone.
pub fn is_transport(e: &zbus::Error) -> bool {
    matches!(
        e,
        zbus::Error::InputOutput(_) | zbus::Error::Handshake(_) | zbus::Error::Address(_)
    )
}
