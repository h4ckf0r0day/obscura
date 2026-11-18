//! Per-page network emulation (`Network.emulateNetworkConditions`).
//!
//! The cell lives in the page's `CallbackRegistry`, not on the HTTP client:
//! pages of one browser context share a client, but conditions are per page
//! as in Chrome. Both transports (reqwest and wreq) and the scripted
//! fetch()/XHR path consult it, so one definition of the behaviour serves all.

use std::sync::RwLock;
use std::time::{Duration, Instant};

use url::Url;

use crate::client::ObscuraNetError;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NetworkConditions {
    pub offline: bool,
    /// Extra round-trip time added before every request hop.
    pub latency: Duration,
    /// Response body throughput cap; `None` is unlimited.
    pub download_bytes_per_sec: Option<u64>,
    /// Request body throughput cap; `None` is unlimited.
    pub upload_bytes_per_sec: Option<u64>,
}

#[derive(Default)]
pub struct NetworkConditionsCell {
    conditions: RwLock<NetworkConditions>,
}

fn transfer_time(bytes: usize, bytes_per_sec: Option<u64>) -> Duration {
    match bytes_per_sec {
        Some(limit) if limit > 0 => Duration::from_secs_f64(bytes as f64 / limit as f64),
        _ => Duration::ZERO,
    }
}

impl NetworkConditionsCell {
    pub fn set(&self, conditions: NetworkConditions) {
        *self.conditions.write().unwrap_or_else(|e| e.into_inner()) = conditions;
    }

    pub fn get(&self) -> NetworkConditions {
        *self.conditions.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs once per request hop, before the transport is touched. Fails when
    /// the page is offline; otherwise waits out the latency and the time the
    /// request body would take at the upload limit.
    pub async fn before_request(
        &self,
        url: &Url,
        upload_bytes: usize,
    ) -> Result<(), ObscuraNetError> {
        let conditions = self.get();
        if conditions.offline {
            return Err(ObscuraNetError::Network(format!(
                "net::ERR_INTERNET_DISCONNECTED: {url}"
            )));
        }
        let delay = conditions.latency
            + transfer_time(upload_bytes, conditions.upload_bytes_per_sec);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        Ok(())
    }

    /// Holds a body read back so `received_bytes` never arrive faster than the
    /// download limit allows since `started`. Called after each chunk with the
    /// running total, so a limit changed mid-download takes effect.
    pub async fn pace_download(&self, started: Instant, received_bytes: usize) {
        let due = transfer_time(received_bytes, self.get().download_bytes_per_sec);
        if let Some(wait) = due.checked_sub(started.elapsed()) {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("http://example.test/").unwrap()
    }

    #[tokio::test]
    async fn default_cell_is_unrestricted() {
        let cell = NetworkConditionsCell::default();
        assert_eq!(cell.get(), NetworkConditions::default());
        let started = Instant::now();
        cell.before_request(&url(), 1_000_000).await.unwrap();
        cell.pace_download(started, 1_000_000).await;
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn offline_fails_with_the_chrome_error() {
        let cell = NetworkConditionsCell::default();
        cell.set(NetworkConditions { offline: true, ..Default::default() });
        let error = cell.before_request(&url(), 0).await.unwrap_err().to_string();
        assert!(error.contains("net::ERR_INTERNET_DISCONNECTED: http://example.test/"), "{error}");
    }

    #[tokio::test]
    async fn latency_and_upload_limit_delay_the_request() {
        let cell = NetworkConditionsCell::default();
        cell.set(NetworkConditions {
            latency: Duration::from_millis(100),
            upload_bytes_per_sec: Some(1_000),
            ..Default::default()
        });
        let started = Instant::now();
        cell.before_request(&url(), 100).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(200), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn download_limit_paces_to_the_byte_budget() {
        let cell = NetworkConditionsCell::default();
        cell.set(NetworkConditions {
            download_bytes_per_sec: Some(1_000),
            ..Default::default()
        });
        let started = Instant::now();
        cell.pace_download(started, 200).await;
        assert!(started.elapsed() >= Duration::from_millis(190), "{:?}", started.elapsed());
    }
}
