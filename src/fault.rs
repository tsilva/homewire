//! Shared bounded fault confirmation and durable deduplication for device adapters.
use crate::{
    Result,
    config::{lock_configuration, read_bounded, save_private},
    error,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub devices: BTreeMap<String, Saved>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub endpoint: String,
    pub faults: Vec<String>,
}
#[derive(Clone)]
struct Candidate {
    faults: Vec<String>,
    since: u64,
}
#[derive(Clone, Default)]
pub struct Monitor {
    pub memory: Memory,
    candidates: BTreeMap<String, Candidate>,
}
#[derive(Serialize)]
pub struct Event {
    pub device: String,
    pub event: &'static str,
    pub faults: Vec<String>,
    pub previous_faults: Vec<String>,
}
// Borrow single reported faults without constructing a temporary Vec/String.
pub(crate) enum Faults<'a> {
    Unknown,
    /// No new sample yet; preserve confirmation but do not advance it.
    NoSample,
    Clear,
    One(&'a str),
    Many(&'a [String]),
}
impl<'a> Faults<'a> {
    pub(crate) fn flag(reported: Option<bool>, name: &'a str) -> Self {
        match reported {
            None => Self::Unknown,
            Some(false) => Self::Clear,
            Some(true) => Self::One(name),
        }
    }
    fn iter(&self) -> impl Iterator<Item = &str> {
        let (one, many) = match self {
            Self::One(name) => (Some(*name), &[][..]),
            Self::Many(names) => (None, *names),
            _ => (None, &[][..]),
        };
        one.into_iter().chain(many.iter().map(String::as_str))
    }
    fn matches(&self, saved: &[String]) -> bool {
        self.iter().eq(saved.iter().map(String::as_str))
    }
    fn owned(&self) -> Vec<String> {
        self.iter().map(str::to_owned).collect()
    }
}
struct Transition {
    event: Event,
    endpoint: String,
}
impl Transition {
    fn publish(&self, memory: &mut Memory) {
        memory.devices.insert(
            self.event.device.clone(),
            Saved {
                endpoint: self.endpoint.clone(),
                faults: self.event.faults.clone(),
            },
        );
    }
}
impl Monitor {
    pub fn observe_faults(
        &mut self,
        name: &str,
        identity: &str,
        faults: Option<&[String]>,
        now: u64,
    ) -> Option<Event> {
        let transition = self.prepare(
            name,
            || identity.to_owned(),
            faults.map_or(Faults::Unknown, Faults::Many),
            now,
        )?;
        transition.publish(&mut self.memory);
        self.candidates.remove(name);
        Some(transition.event)
    }
    // Confirmation updates only candidates. Published memory changes after the
    // whole batch has been durably saved, so normal polls need no rollback copy.
    fn prepare(
        &mut self,
        name: &str,
        identity: impl FnOnce() -> String,
        faults: Faults<'_>,
        now: u64,
    ) -> Option<Transition> {
        if matches!(faults, Faults::NoSample) {
            return None;
        }
        if matches!(faults, Faults::Unknown) {
            self.candidates.remove(name);
            return None;
        }
        let published = self
            .memory
            .devices
            .get(name)
            .map(|s| s.faults.as_slice())
            .unwrap_or(&[]);
        if faults.matches(published) {
            self.candidates.remove(name);
            return None;
        }
        if !self
            .candidates
            .get(name)
            .is_some_and(|c| faults.matches(&c.faults))
        {
            self.candidates.insert(
                name.to_owned(),
                Candidate {
                    faults: faults.owned(),
                    since: now,
                },
            );
            return None;
        }
        let candidate = self.candidates.get(name)?;
        let delay = if candidate.faults.is_empty() { 30 } else { 3 };
        if now.saturating_sub(candidate.since) < delay {
            return None;
        }
        Some(Transition {
            event: Event {
                device: name.to_owned(),
                event: if candidate.faults.is_empty() {
                    "recovered"
                } else {
                    "fault"
                },
                faults: candidate.faults.clone(),
                previous_faults: published.to_vec(),
            },
            endpoint: identity(),
        })
    }
}

impl Monitor {
    pub fn from_memory(memory: Memory) -> Self {
        Self {
            memory,
            ..Self::default()
        }
    }
    pub fn load(path: &Path, identities: &BTreeMap<String, String>) -> Result<Self> {
        Self::load_matching(path, |name, endpoint| {
            identities.get(name).is_some_and(|id| id == endpoint)
        })
    }
    fn load_matching(path: &Path, matches: impl Fn(&str, &str) -> bool) -> Result<Self> {
        let mut memory: Memory = match read_bounded(path, "Device alert memory") {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| error("Invalid device alert memory"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Memory::default(),
            Err(e) => return Err(e.into()),
        };
        memory
            .devices
            .retain(|name, saved| matches(name, &saved.endpoint));
        if memory
            .devices
            .values()
            .any(|s| s.faults.len() > 64 || s.faults.iter().any(|f| f.len() > 128))
        {
            return Err(error("Invalid device alert memory bounds"));
        }
        Ok(Self::from_memory(memory))
    }
    pub fn active_faults(&self) -> BTreeMap<String, Vec<String>> {
        self.memory
            .devices
            .iter()
            .filter(|(_, s)| !s.faults.is_empty())
            .map(|(name, s)| (name.clone(), s.faults.clone()))
            .collect()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        save_private(path, &self.memory)
    }
}

/// Common result shape. Adapter-specific event/unavailable names remain intact.
#[derive(Serialize)]
pub struct Check<S, U, E = Event> {
    pub events: Vec<E>,
    pub statuses: Vec<S>,
    pub unavailable: Vec<U>,
    pub active_faults: BTreeMap<String, Vec<String>>,
}

/// Background monitoring needs only transitions and unavailable devices.
pub struct Watch<U, E = Event> {
    pub events: Vec<E>,
    pub unavailable: Vec<U>,
}

// Static dispatch keeps device decoding and safety rules in each adapter; this
// module owns polling, unknown readings, confirmation and durable publication.
pub(crate) trait Target {
    type Status;
    type Unavailable;
    type Event: From<Event>;
    fn name(&self) -> &str;
    fn identity(&self) -> String;
    fn read(&self, timeout: Duration) -> Result<Self::Status>;
    fn faults(status: &Self::Status) -> Faults<'_>;
    /// Threshold warnings can use published state for hysteresis without
    /// retaining a second device snapshot or bypassing confirmation/persistence.
    fn monitored_faults<'a>(&self, status: &'a Self::Status, _published: &[String]) -> Faults<'a> {
        Self::faults(status)
    }
    fn unavailable(&self, error: String) -> Self::Unavailable;
}

pub(crate) struct Store {
    state_path: PathBuf,
    _lock: File,
    started: Instant,
}
impl Store {
    pub(crate) fn open<T: Target>(path: &Path, targets: &[T]) -> Result<(Self, Monitor)> {
        let lock = lock_configuration(path)?;
        let state_path = path.with_extension("state.json");
        let monitor = Monitor::load_matching(&state_path, |name, identity| {
            targets
                .iter()
                .any(|t| t.name() == name && t.identity() == identity)
        })?;
        Ok((
            Self {
                state_path,
                _lock: lock,
                started: Instant::now(),
            },
            monitor,
        ))
    }
    pub(crate) fn check<T: Target>(
        &self,
        monitor: &mut Monitor,
        targets: &[T],
        timeout: Duration,
    ) -> Result<Check<T::Status, T::Unavailable, T::Event>> {
        self.poll(monitor, targets, timeout, true)
    }
    pub(crate) fn watch<T: Target>(
        &self,
        monitor: &mut Monitor,
        targets: &[T],
        timeout: Duration,
    ) -> Result<Watch<T::Unavailable, T::Event>> {
        let check = self.poll(monitor, targets, timeout, false)?;
        Ok(Watch {
            events: check.events,
            unavailable: check.unavailable,
        })
    }
    fn poll<T: Target>(
        &self,
        monitor: &mut Monitor,
        targets: &[T],
        timeout: Duration,
        snapshot: bool,
    ) -> Result<Check<T::Status, T::Unavailable, T::Event>> {
        let mut check = Check {
            events: vec![],
            statuses: vec![],
            unavailable: vec![],
            active_faults: BTreeMap::new(),
        };
        let mut transitions = Vec::new();
        for target in targets {
            let result = target.read(timeout);
            let published = monitor
                .memory
                .devices
                .get(target.name())
                .map_or(&[][..], |saved| saved.faults.as_slice());
            let faults = result.as_ref().map_or(Faults::Unknown, |status| {
                target.monitored_faults(status, published)
            });
            if let Some(transition) = monitor.prepare(
                target.name(),
                || target.identity(),
                faults,
                self.started.elapsed().as_secs(),
            ) {
                transitions.push(transition);
            }
            match result {
                Ok(status) => {
                    if snapshot {
                        check.statuses.push(status);
                    }
                }
                Err(e) => check.unavailable.push(target.unavailable(e.to_string())),
            }
        }
        if !transitions.is_empty() {
            let mut next = monitor.memory.clone();
            for transition in &transitions {
                transition.publish(&mut next);
            }
            // Failed persistence leaves published memory untouched and emits no
            // events. Confirmed candidates remain available for the next poll.
            save_private(&self.state_path, &next)?;
            monitor.memory = next;
            for transition in transitions {
                monitor.candidates.remove(&transition.event.device);
                check.events.push(transition.event.into());
            }
        }
        if snapshot {
            check.active_faults = monitor.active_faults();
        }
        Ok(check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_telemetry_cannot_confirm_a_transition() {
        let mut monitor = Monitor::default();
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::One("low_battery"), 0)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::NoSample, 100)
                .is_none()
        );
        let low = monitor
            .prepare("phone", || "phone".into(), Faults::One("low_battery"), 101)
            .expect("new low report");
        low.publish(&mut monitor.memory);
        monitor.candidates.clear();
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::Clear, 102)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::NoSample, 140)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::Unknown, 141)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::Clear, 142)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::Clear, 171)
                .is_none()
        );
        assert!(
            monitor
                .prepare("phone", || "phone".into(), Faults::Clear, 172)
                .is_some()
        );
    }
    use std::{
        cell::Cell,
        fs,
        sync::atomic::{AtomicUsize, Ordering},
        time::SystemTime,
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Stub {
        name: &'static str,
        reported: Cell<Option<bool>>,
        reachable: Cell<bool>,
        identities: Cell<usize>,
    }
    impl Stub {
        fn new(name: &'static str, reported: Option<bool>) -> Self {
            Self {
                name,
                reported: Cell::new(reported),
                reachable: Cell::new(true),
                identities: Cell::new(0),
            }
        }
    }
    impl Target for Stub {
        type Status = Option<bool>;
        type Unavailable = String;
        type Event = Event;
        fn name(&self) -> &str {
            self.name
        }
        fn identity(&self) -> String {
            self.identities.set(self.identities.get() + 1);
            format!("synthetic-{}", self.name)
        }
        fn read(&self, _: Duration) -> Result<Self::Status> {
            if self.reachable.get() {
                Ok(self.reported.get())
            } else {
                Err(error("unreachable"))
            }
        }
        fn faults(status: &Self::Status) -> Faults<'_> {
            Faults::flag(*status, "reported-fault")
        }
        fn unavailable(&self, error: String) -> Self::Unavailable {
            error
        }
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Result<Self> {
            let dir = std::env::temp_dir().join(format!(
                "homewire-fault-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)?
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&dir)?;
            Ok(Self(dir))
        }
        fn config(&self) -> PathBuf {
            self.0.join("devices.json")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn at(store: &mut Store, seconds: u64) {
        store.started = Instant::now()
            .checked_sub(Duration::from_secs(seconds))
            .expect("clock");
    }

    #[test]
    fn failed_batch_save_publishes_nothing_and_retry_is_durable() -> Result<()> {
        let temp = Temp::new()?;
        let targets = [
            Stub::new("first", Some(true)),
            Stub::new("second", Some(true)),
        ];
        let (mut store, mut monitor) = Store::open(&temp.config(), &targets)?;
        assert!(
            store
                .check(&mut monitor, &targets, Duration::ZERO)?
                .events
                .is_empty()
        );
        // A directory at the final state path forces atomic rename to fail.
        fs::create_dir(&store.state_path)?;
        at(&mut store, 3);
        assert!(store.check(&mut monitor, &targets, Duration::ZERO).is_err());
        assert!(monitor.memory.devices.is_empty());
        assert!(
            !store
                .state_path
                .with_extension(format!("json.tmp-{}", std::process::id()))
                .exists()
        );
        fs::remove_dir(&store.state_path)?;
        let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
        assert_eq!(check.events.len(), 2);
        let persisted: Memory = serde_json::from_slice(&fs::read(&store.state_path)?)?;
        assert_eq!(persisted.devices.len(), 2);
        for event in check.events {
            assert_eq!(persisted.devices[&event.device].faults, event.faults);
        }
        drop(store);
        let (store, mut monitor) = Store::open(&temp.config(), &targets)?;
        assert!(
            store
                .check(&mut monitor, &targets, Duration::ZERO)?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn unchanged_polls_do_not_build_identities_or_write_state() -> Result<()> {
        let temp = Temp::new()?;
        let targets = [Stub::new("clear", Some(false))];
        let (store, mut monitor) = Store::open(&temp.config(), &targets)?;
        fs::create_dir(&store.state_path)?;
        for _ in 0..100 {
            let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
            assert!(check.events.is_empty());
            assert_eq!(check.statuses.len(), 1);
        }
        assert_eq!(targets[0].identities.get(), 0);
        assert!(monitor.memory.devices.is_empty());
        Ok(())
    }

    #[test]
    fn watch_and_snapshot_checks_share_durable_deduplication() -> Result<()> {
        let temp = Temp::new()?;
        let targets = [Stub::new("device", Some(true))];
        let (mut store, mut monitor) = Store::open(&temp.config(), &targets)?;
        assert!(
            store
                .watch(&mut monitor, &targets, Duration::ZERO)?
                .events
                .is_empty()
        );
        at(&mut store, 3);
        let watch = store.watch(&mut monitor, &targets, Duration::ZERO)?;
        assert_eq!(watch.events.len(), 1);
        assert!(watch.unavailable.is_empty());
        let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
        assert!(check.events.is_empty());
        assert_eq!(check.statuses.len(), 1);
        assert_eq!(check.active_faults.len(), 1);
        targets[0].reachable.set(false);
        let watch = store.watch(&mut monitor, &targets, Duration::ZERO)?;
        assert_eq!(watch.unavailable.len(), 1);
        assert!(watch.events.is_empty());
        assert_eq!(monitor.active_faults().len(), 1);
        Ok(())
    }

    #[test]
    fn unknown_or_unavailable_readings_cancel_pending_recovery() -> Result<()> {
        let temp = Temp::new()?;
        let targets = [Stub::new("device", Some(true))];
        let (mut store, mut monitor) = Store::open(&temp.config(), &targets)?;
        store.check(&mut monitor, &targets, Duration::ZERO)?;
        at(&mut store, 3);
        assert_eq!(
            store
                .check(&mut monitor, &targets, Duration::ZERO)?
                .events
                .len(),
            1
        );
        targets[0].reported.set(Some(false));
        at(&mut store, 10);
        store.check(&mut monitor, &targets, Duration::ZERO)?;
        targets[0].reported.set(None);
        at(&mut store, 40);
        let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
        assert!(check.events.is_empty());
        assert_eq!(check.active_faults.len(), 1);
        targets[0].reported.set(Some(false));
        store.check(&mut monitor, &targets, Duration::ZERO)?;
        targets[0].reachable.set(false);
        at(&mut store, 70);
        let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
        assert_eq!(check.unavailable.len(), 1);
        assert_eq!(check.active_faults.len(), 1);
        targets[0].reachable.set(true);
        store.check(&mut monitor, &targets, Duration::ZERO)?;
        at(&mut store, 99);
        assert!(
            store
                .check(&mut monitor, &targets, Duration::ZERO)?
                .events
                .is_empty()
        );
        at(&mut store, 100);
        let check = store.check(&mut monitor, &targets, Duration::ZERO)?;
        assert_eq!(check.events[0].event, "recovered");
        assert!(check.active_faults.is_empty());
        Ok(())
    }
}
