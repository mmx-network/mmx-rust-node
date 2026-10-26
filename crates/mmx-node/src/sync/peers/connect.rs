//! Bounded background connection attempts. A slow seed must not hold up the
//! existing sync pool while the configured outbound target is replenished.
use super::*;
use std::thread::JoinHandle;

pub(super) struct Connections<S> {
    jobs: BTreeMap<String, JoinHandle<Result<S>>>,
    maximum: usize,
    fixed: BTreeSet<String>,
    retry: Duration,
    interval: Duration,
    attempted: Option<Instant>,
    probe: Option<String>,
    probed: Option<Instant>,
    probe_interval: Option<Duration>,
}
impl<S: Send + 'static> Connections<S> {
    pub fn new() -> Self {
        Self {
            jobs: BTreeMap::new(),
            maximum: MAX_PEERS,
            fixed: BTreeSet::new(),
            retry: Duration::from_secs(30),
            interval: Duration::ZERO,
            attempted: None,
            probe: None,
            probed: None,
            probe_interval: None,
        }
    }
    pub fn with_config(config: &crate::router::RouterConfig) -> Self {
        let mut value = Self::new();
        value.maximum = config.num_peers_out;
        value.fixed = config.fixed_peers.clone();
        value.retry = config.peer_retry_interval;
        value.interval = config.connect_interval;
        value.probe_interval = config.discover_interval;
        value
    }
    pub fn refill(
        &mut self,
        active: usize,
        addresses: &[String],
        connected: &BTreeMap<Hash, String>,
        retry_at: &mut BTreeMap<String, Instant>,
        connect: impl Fn(&str) -> Result<S> + Send + Sync + 'static,
    ) -> Result<()> {
        let now = Instant::now();
        if self
            .attempted
            .is_some_and(|at| now.duration_since(at) < self.interval)
        {
            return Ok(());
        }
        let fixed_active = connected
            .values()
            .filter(|address| self.fixed.contains(*address))
            .count();
        let ordinary_pending = self
            .jobs
            .keys()
            .filter(|address| !self.fixed.contains(*address))
            .count();
        // Native connect() bypasses retry deadlines until enough ordinary
        // synced peers exist. A failed seed must not delay cold startup for
        // the default six-hour full-pool recheck period.
        let underfilled = active.saturating_sub(fixed_active) < self.maximum;
        let slots = self
            .maximum
            .saturating_sub(active.saturating_sub(fixed_active) + ordinary_pending);
        let mut candidates: Vec<_> = addresses
            .iter()
            .filter(|address| {
                !self.jobs.contains_key(*address)
                    && !connected.values().any(|a| a == *address)
                    && (self.fixed.contains(*address)
                        || underfilled
                        || retry_at
                            .get(*address)
                            .is_none_or(|deadline| now >= *deadline))
            })
            .cloned()
            .collect();
        candidates.sort();
        candidates.dedup();
        candidates.shuffle(&mut rand::thread_rng());
        let (mut fixed, mut ordinary): (Vec<_>, Vec<_>) = candidates
            .into_iter()
            .partition(|address| self.fixed.contains(address));
        let probe = slots == 0
            && active.saturating_sub(fixed_active) >= self.maximum
            && ordinary_pending == 0
            && self.probe.is_none()
            && self.probe_interval.is_some_and(|interval| {
                self.probed
                    .is_none_or(|at| now.duration_since(at) >= interval)
            });
        ordinary.truncate(slots.max(usize::from(probe)));
        if probe {
            if let Some(address) = ordinary.first() {
                self.probe = Some(address.clone());
                self.probed = Some(now);
            }
        }
        fixed.extend(ordinary);
        let candidates = fixed;
        if candidates.is_empty() {
            return Ok(());
        }
        self.attempted = Some(now);
        let connect = Arc::new(connect);
        for address in candidates {
            let endpoint = address.clone();
            let connect = connect.clone();
            let job = match thread::Builder::new()
                .name("mmx-peer-connect".into())
                .spawn(move || connect(&endpoint))
            {
                Ok(job) => job,
                Err(failure) if self.probe.as_ref() == Some(&address) => {
                    self.probe = None;
                    eprintln!("Peer probe {address}: {failure}");
                    continue;
                }
                Err(failure) => return Err(failure.into()),
            };
            retry_at.insert(address.clone(), now + self.retry);
            self.jobs.insert(address, job);
        }
        Ok(())
    }
    pub fn finished(&mut self) -> Vec<(String, Result<S>)> {
        let ready: Vec<_> = self
            .jobs
            .iter()
            .filter(|(_, job)| job.is_finished())
            .map(|(address, _)| address.clone())
            .collect();
        ready
            .into_iter()
            .map(|address| {
                if self.probe.as_ref() == Some(&address) {
                    self.probe = None;
                }
                let result = self
                    .jobs
                    .remove(&address)
                    .unwrap()
                    .join()
                    .unwrap_or_else(|_| Err(error("peer connection worker panicked")));
                (address, result)
            })
            .collect()
    }
    pub fn pending(&self, address: &str) -> bool {
        self.jobs.contains_key(address)
    }
}
impl<S> Drop for Connections<S> {
    fn drop(&mut self) {
        for (_, job) in std::mem::take(&mut self.jobs) {
            let _ = job.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_fixed_peers_are_extra_slots_and_ignore_ordinary_retry_backoff() {
        let config = crate::router::RouterConfig {
            num_peers_out: 1,
            min_sync_peers: 1,
            fixed_peers: ["fixed".to_owned()].into(),
            connect_interval: Duration::ZERO,
            discover_interval: None,
            ..Default::default()
        };
        let mut pending = Connections::<String>::with_config(&config);
        let connected = [([1; 32], "ordinary".to_owned())].into();
        let addresses = vec!["ordinary".into(), "fixed".into(), "another".into()];
        let mut retries = [("fixed".into(), Instant::now() + Duration::from_secs(3600))].into();
        pending
            .refill(1, &addresses, &connected, &mut retries, |address| {
                Ok(address.to_owned())
            })
            .unwrap();
        assert!(pending.jobs.contains_key("fixed"));
        assert!(!pending.jobs.contains_key("another"));
        assert!(retries["fixed"] >= Instant::now() + Duration::from_secs(3500));
        // The configured 360-minute interval is stored, but fixed peers may
        // try again on a later connect cycle without waiting for this entry.
        let mut pending = Connections::<String>::with_config(&config);
        let connected = [
            ([1; 32], "ordinary".to_owned()),
            ([2; 32], "fixed".to_owned()),
        ]
        .into();
        pending
            .refill(2, &addresses, &connected, &mut retries, |_| {
                panic!("all ordinary and fixed slots are occupied")
            })
            .unwrap();
        assert!(pending.jobs.is_empty());
    }
    #[test]
    fn configured_connect_period_throttles_new_attempts_but_never_hides_completions() {
        let config = crate::router::RouterConfig {
            num_peers_out: 2,
            min_sync_peers: 1,
            connect_interval: Duration::from_secs(60),
            peer_retry_interval: Duration::from_secs(120),
            ..Default::default()
        };
        let mut pending = Connections::with_config(&config);
        let addresses = vec!["one".into(), "two".into()];
        let mut retries = BTreeMap::new();
        let connected = BTreeMap::new();
        pending
            .refill(0, &addresses, &connected, &mut retries, |address| {
                Ok(address.to_owned())
            })
            .unwrap();
        let mut done = vec![];
        let until = Instant::now() + Duration::from_secs(3);
        while done.len() < 2 && Instant::now() < until {
            done.extend(pending.finished());
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            done.len(),
            2,
            "completed connections are delivered inside the connect interval"
        );
        retries.clear();
        pending
            .refill(0, &addresses, &connected, &mut retries, |_| {
                panic!("connect cycle is not due")
            })
            .unwrap();
        assert!(pending.jobs.is_empty());
    }
    #[test]
    fn an_underfilled_pool_retries_failed_seeds_without_waiting_for_full_pool_backoff() {
        let config = crate::router::RouterConfig {
            num_peers_out: 1,
            min_sync_peers: 1,
            ..Default::default()
        };
        let mut pending = Connections::with_config(&config);
        let address = "seed".to_owned();
        let mut retries = [(address.clone(), Instant::now() + config.peer_retry_interval)].into();
        pending
            .refill(
                0,
                std::slice::from_ref(&address),
                &BTreeMap::new(),
                &mut retries,
                |peer| Ok(peer.to_owned()),
            )
            .unwrap();
        assert!(pending.jobs.contains_key(&address));
        assert_eq!(pending.retry, Duration::from_secs(360 * 60));
    }
    #[test]
    fn a_full_pool_has_only_one_optional_probe_and_keeps_normal_retry_and_interval_bounds() {
        let policy = crate::router::RouterConfig {
            num_peers_out: 2,
            min_sync_peers: 1,
            connect_interval: Duration::ZERO,
            discover_interval: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        let mut pending = Connections::with_config(&policy);
        let connected = [([1; 32], "one".to_owned()), ([2; 32], "two".to_owned())].into();
        let addresses = vec![
            "one".into(),
            "two".into(),
            "three".into(),
            "four".into(),
            "backoff".into(),
        ];
        let mut retries = [("backoff".into(), Instant::now() + Duration::from_secs(3600))].into();
        pending
            .refill(2, &addresses, &connected, &mut retries, |address| {
                Ok(address.to_owned())
            })
            .unwrap();
        assert_eq!(pending.jobs.len(), 1);
        assert!(!pending.jobs.contains_key("backoff"));
        pending
            .refill(
                2,
                &addresses,
                &connected,
                &mut retries,
                |_| -> Result<String> { panic!("only one probe may be pending") },
            )
            .unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while pending.finished().is_empty() {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
        pending
            .refill(
                2,
                &addresses,
                &connected,
                &mut retries,
                |_| -> Result<String> { panic!("probe interval has not elapsed") },
            )
            .unwrap();
        assert!(pending.jobs.is_empty());
    }
}
