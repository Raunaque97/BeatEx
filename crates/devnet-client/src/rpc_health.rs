use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Endpoint {
    failures: u32,
    retry_at: Option<Instant>,
}

impl Endpoint {
    fn fail(&mut self, rate_limited: bool, retry_after: Option<Duration>, now: Instant) {
        let delay = if rate_limited {
            retry_after
                .unwrap_or(Duration::from_secs(60))
                .clamp(Duration::from_secs(1), Duration::from_secs(300))
        } else {
            Duration::from_secs(1u64 << self.failures.min(5))
        };
        self.failures = self.failures.saturating_add(1);
        self.retry_at = Some(now + delay);
    }
    fn ready(&self, now: Instant) -> bool {
        self.retry_at.is_none_or(|retry_at| now >= retry_at)
    }
}

fn endpoints() -> &'static Mutex<HashMap<String, Endpoint>> {
    static HEALTH: OnceLock<Mutex<HashMap<String, Endpoint>>> = OnceLock::new();
    HEALTH.get_or_init(Mutex::default)
}

pub(super) fn ready(url: &str) -> bool {
    endpoints()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(url)
        .is_none_or(|endpoint| endpoint.ready(Instant::now()))
}
pub(super) fn failed(url: &str, rate_limited: bool, retry_after: Option<Duration>) {
    endpoints()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .entry(url.to_owned())
        .or_default()
        .fail(rate_limited, retry_after, Instant::now());
}
pub(super) fn succeeded(url: &str) {
    endpoints()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(url);
}

type SlotCache = HashMap<(Vec<String>, String), (Instant, Vec<u8>)>;
fn slot_cache() -> &'static Mutex<SlotCache> {
    static CACHE: OnceLock<Mutex<SlotCache>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

pub(super) fn cached_slot(endpoints: &[String], request: &str) -> Option<Vec<u8>> {
    let key = (endpoints.to_vec(), request.to_owned());
    slot_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
        .filter(|(time, _)| time.elapsed() < Duration::from_millis(500))
        .map(|(_, body)| body.clone())
}

pub(super) fn cache_slot(endpoints: &[String], request: &str, body: Vec<u8>) {
    let mut cache = slot_cache()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    cache.retain(|_, (time, _)| time.elapsed() < Duration::from_millis(500));
    cache.insert(
        (endpoints.to_vec(), request.to_owned()),
        (Instant::now(), body),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_limits_honor_retry_after_and_transport_failures_back_off() {
        let now = Instant::now();
        let mut endpoint = Endpoint::default();
        endpoint.fail(true, Some(Duration::from_secs(90)), now);
        assert!(!endpoint.ready(now + Duration::from_secs(89)));
        assert!(endpoint.ready(now + Duration::from_secs(90)));
        let mut endpoint = Endpoint::default();
        for seconds in [1, 2, 4, 8, 16, 32, 32] {
            endpoint.fail(false, None, now);
            assert_eq!(
                endpoint.retry_at.unwrap().duration_since(now),
                Duration::from_secs(seconds)
            );
        }
    }
}
