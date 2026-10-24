//! One shared curve and bounded, timer-free physical-device execution lanes.
use crate::{
    Result,
    brightness::{Cycle, State},
    device::{BrightnessDevice, Devices},
    entity, error,
    light_schedule::Config,
};
use chrono::Utc;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const MAX_TARGETS: usize = 9;
trait Execute: Send + 'static {
    fn run(&self, state: &mut State, cycle: &Cycle) -> Result<bool>;
}
impl Execute for BrightnessDevice {
    fn run(&self, state: &mut State, cycle: &Cycle) -> Result<bool> {
        BrightnessDevice::run(self, state, cycle)
    }
}
struct Worker<D = BrightnessDevice> {
    entity: String,
    control: crate::brightness::Control,
    payload: Option<(D, State)>,
    sender: Option<SyncSender<Cycle>>,
    receiver: Option<Receiver<Result<bool>>>,
    thread: Option<JoinHandle<()>>,
    busy: bool,
    failed: bool,
}
impl<D: Execute> Worker<D> {
    fn dispatch(&mut self, cycle: &Cycle, stop: &Arc<AtomicBool>) -> Result<bool> {
        let mut applied = false;
        if self.sender.is_none() {
            let (device, mut state) = self
                .payload
                .take()
                .ok_or_else(|| error("Brightness worker unavailable"))?;
            let (tx, rx) = mpsc::sync_channel::<Cycle>(1);
            let (done, result) = mpsc::sync_channel(1);
            let stop = stop.clone();
            self.thread = Some(
                thread::Builder::new()
                    .name("brightness-io".into())
                    .stack_size(256 * 1024)
                    .spawn(move || {
                        while let Ok(cycle) = rx.recv() {
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            let result = if cycle.fresh() {
                                device.run(&mut state, &cycle)
                            } else {
                                Ok(false)
                            };
                            if done.try_send(result).is_err() {
                                break;
                            }
                        }
                    })?,
            );
            self.sender = Some(tx);
            self.receiver = Some(result);
        }
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.busy = false;
                    self.failed = result.is_err();
                    match result {
                        Ok(true) => {
                            applied = true;
                            eprintln!("Brightness schedule verified: {}", self.entity);
                        }
                        Ok(false) => {}
                        Err(e) => eprintln!(
                            "Brightness schedule failed for {} (no replay): {e}",
                            self.entity
                        ),
                    }
                }
                Err(TryRecvError::Disconnected) => {
                    return Err(error("Brightness worker stopped; no replay"));
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        // One in-flight cycle, no successor queued behind a slow device.
        if !self.busy {
            self.sender
                .as_ref()
                .ok_or_else(|| error("Missing brightness worker"))?
                .try_send(cycle.clone())
                .map_err(|_| error("Brightness worker unavailable; no replay"))?;
            self.busy = true;
        }
        Ok(applied)
    }
}
pub struct Coordinator {
    curve: Config,
    pub poll_seconds: u64,
    workers: Vec<Worker>,
    stop: Arc<AtomicBool>,
}
impl Coordinator {
    pub fn from_devices(devices: &mut Devices) -> Result<Option<Self>> {
        let Some(lights) = devices.bulbs.as_ref() else {
            return Ok(None);
        };
        let Some(curve) = &lights.config.automation else {
            return Ok(None);
        };
        let curve = curve.clone();
        let source = lights.schedule_source.clone();
        let poll_seconds = lights.config.poll_seconds;
        let now = Utc::now();
        let mut selected = Vec::with_capacity(MAX_TARGETS);
        let mut jobs = vec![(format!("light.{}", curve.light), true)];
        for name in &curve.brightness_lights {
            jobs.push((format!("light.{name}"), false));
        }
        if curve.follow_tv_backlight {
            jobs.push(("media_player.tcl".into(), false));
        }
        for name in &curve.brightness_entities {
            jobs.push((name.clone(), false));
        }
        if jobs.len() > MAX_TARGETS {
            return Err(error(
                "At most nine scheduled brightness devices are supported",
            ));
        }
        let mut workers = Vec::with_capacity(jobs.len());
        for (id, primary) in jobs {
            let descriptor = entity::resolve(devices, &id)?;
            if descriptor.field.is_some() {
                return Err(error(
                    "A brightness follower must target a physical-device entity",
                ));
            }
            if selected.contains(&descriptor.device) {
                return Err(error("Duplicate scheduled physical device"));
            }
            if primary && descriptor.owner.is_some() {
                return Err(error("A routine-owned device cannot be the sunset target"));
            }
            let route = descriptor.device;
            let device = devices
                .brightness_device(route)?
                .ok_or_else(|| error("Scheduled entity has no brightness execution capability"))?;
            let path = source.with_extension(if primary {
                "schedule.state.json".into()
            } else {
                device.state_key()
            });
            let state = State::open(&path, &device.identity(), primary, now)?;
            devices.set_brightness_control(route, state.control.clone())?;
            selected.push(route);
            workers.push(Worker {
                entity: id,
                control: state.control.clone(),
                payload: Some((device, state)),
                sender: None,
                receiver: None,
                thread: None,
                busy: false,
                failed: false,
            });
        }
        Ok(Some(Self {
            curve,
            poll_seconds,
            workers,
            stop: Arc::new(AtomicBool::new(false)),
        }))
    }
    pub fn wake_requested(&self) -> bool {
        self.workers.iter().any(|w| w.control.wake_requested())
    }
    /// Completed results feed the existing schedule alert stream on the next cycle.
    pub fn tick(&mut self, deadline: Instant) -> Result<Option<bool>> {
        let cycle = Cycle::new(&self.curve, Utc::now(), deadline)?;
        let mut applied = false;
        for worker in &mut self.workers {
            worker.control.take_wake();
            match worker.dispatch(&cycle, &self.stop) {
                Ok(changed) => applied |= changed,
                Err(e) => {
                    worker.failed = true;
                    eprintln!("Brightness dispatch failed for {}: {e}", worker.entity);
                }
            }
        }
        let failed = self.workers.iter().any(|w| w.failed);
        Ok((failed || applied).then_some(failed))
    }
}
impl Drop for Coordinator {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for worker in &mut self.workers {
            worker.sender.take();
        }
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
    }
}
/// Preserve monotonic phase and skip missed slots instead of drifting or catching up.
pub fn next_deadline(previous: Instant, now: Instant, period: Duration) -> Instant {
    if previous > now {
        return previous;
    }
    let remainder = now.duration_since(previous).as_nanos() % period.as_nanos();
    now + period - Duration::from_nanos(remainder as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brightness::{Io, Observation};
    use std::sync::atomic::AtomicUsize;
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }
    struct Probe {
        release: Option<Receiver<()>>,
        entered: SyncSender<()>,
        applied: SyncSender<(chrono::DateTime<Utc>, u8)>,
        calls: Arc<AtomicUsize>,
        current: u8,
    }
    // Each probe is moved into one lane. The receiver needs only Send, not Sync.
    struct TestDevice(std::sync::Mutex<Probe>);
    impl Execute for TestDevice {
        fn run(&self, state: &mut State, cycle: &Cycle) -> Result<bool> {
            let mut probe = self.0.lock().map_err(|_| error("Probe poisoned"))?;
            struct Session<'a> {
                probe: &'a mut Probe,
                at: chrono::DateTime<Utc>,
            }
            impl Io for Session<'_> {
                type Applied = ();
                fn read(&mut self) -> Result<Observation> {
                    self.probe.calls.fetch_add(1, Ordering::SeqCst);
                    let _ = self.probe.entered.try_send(());
                    if let Some(release) = self.probe.release.take() {
                        release.recv().map_err(|_| error("Probe cancelled"))?;
                    }
                    Ok(Observation {
                        power: true,
                        brightness: self.probe.current,
                        temperature: None,
                    })
                }
                fn apply(
                    &mut self,
                    change: &crate::bulb::Change,
                    _: &crate::brightness::Permit,
                ) -> Result<()> {
                    let level = change
                        .brightness_percent
                        .ok_or_else(|| error("Missing target"))?;
                    self.probe
                        .applied
                        .send((self.at, level))
                        .map_err(|_| error("Probe closed"))?;
                    self.probe.current = level;
                    Ok(())
                }
            }
            Ok(state
                .tick(
                    cycle,
                    &mut Session {
                        probe: &mut probe,
                        at: cycle.at,
                    },
                )?
                .is_some())
        }
    }
    fn cycle() -> Result<Cycle> {
        let config = serde_json::from_value(
            serde_json::json!({"light":"strip","timezone":"Etc/UTC","latitude":0,"longitude":0,
            "curve":[[360,10,2700],[480,90,5000]],"outside_curve":[5,2700]}),
        )?;
        Cycle::new(
            &config,
            "2026-10-23T07:00:00Z".parse()?,
            Instant::now() + Duration::from_secs(3),
        )
    }
    type Fixture = (
        Worker<TestDevice>,
        Receiver<()>,
        Receiver<(chrono::DateTime<Utc>, u8)>,
        Arc<AtomicUsize>,
    );
    fn worker(
        temp: &support::Directory,
        name: &str,
        release: Option<Receiver<()>>,
    ) -> Result<Fixture> {
        let (entered, seen) = mpsc::sync_channel(1);
        let (applied, changes) = mpsc::sync_channel(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let device = TestDevice(std::sync::Mutex::new(Probe {
            release,
            entered,
            applied,
            calls: calls.clone(),
            current: 1,
        }));
        let state = State::open(
            &temp.0.join(format!("{name}.json")),
            name,
            false,
            Utc::now(),
        )?;
        Ok((
            Worker {
                entity: name.into(),
                control: state.control.clone(),
                payload: Some((device, state)),
                sender: None,
                receiver: None,
                thread: None,
                busy: false,
                failed: false,
            },
            seen,
            changes,
            calls,
        ))
    }
    fn stop(worker: &mut Worker<TestDevice>) {
        worker.sender.take();
        if let Some(join) = worker.thread.take() {
            join.join().expect("worker finished");
        }
    }
    #[test]
    fn stalled_device_does_not_delay_healthy_device_and_both_receive_identical_target() -> Result<()>
    {
        let temp = support::Directory::new()?;
        let target = cycle()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (release, wait) = mpsc::sync_channel(1);
        let (mut slow, entered, slow_changes, _) = worker(&temp, "slow", Some(wait))?;
        let (mut healthy, _, healthy_changes, _) = worker(&temp, "healthy", None)?;
        slow.dispatch(&target, &stop_flag)?;
        entered.recv_timeout(Duration::from_secs(1))?;
        healthy.dispatch(&target, &stop_flag)?;
        let applied = healthy_changes.recv_timeout(Duration::from_secs(1));
        // Release even if the assertion fails, so no test leaves an I/O lane stuck.
        release.send(())?;
        let other = slow_changes.recv_timeout(Duration::from_secs(1));
        stop(&mut slow);
        stop(&mut healthy);
        assert_eq!(applied?, (target.at, 50));
        assert_eq!(other?, (target.at, 50));
        Ok(())
    }
    #[test]
    fn busy_lane_does_not_queue_an_obsolete_successor() -> Result<()> {
        let temp = support::Directory::new()?;
        let target = cycle()?;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (release, wait) = mpsc::sync_channel(1);
        let (mut lane, entered, changes, calls) = worker(&temp, "slow", Some(wait))?;
        lane.dispatch(&target, &stop_flag)?;
        entered.recv_timeout(Duration::from_secs(1))?;
        let mut obsolete = target.clone();
        obsolete.target.brightness_percent = Some(90);
        lane.dispatch(&obsolete, &stop_flag)?;
        release.send(())?;
        assert_eq!(changes.recv_timeout(Duration::from_secs(1))?.1, 50);
        stop(&mut lane);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(changes.try_recv().is_err());
        Ok(())
    }
    #[test]
    fn expired_target_is_discarded_after_a_slow_read() -> Result<()> {
        let temp = support::Directory::new()?;
        let mut target = cycle()?;
        target.deadline = Instant::now() + Duration::from_millis(30);
        let (mut lane, entered, changes, _) = worker(&temp, "expired", None)?;
        struct SlowRead;
        impl Io for SlowRead {
            type Applied = ();
            fn read(&mut self) -> Result<Observation> {
                thread::sleep(Duration::from_millis(50));
                Ok(Observation {
                    power: true,
                    brightness: 1,
                    temperature: None,
                })
            }
            fn apply(
                &mut self,
                _: &crate::bulb::Change,
                _: &crate::brightness::Permit,
            ) -> Result<()> {
                panic!("Expired target sent a control")
            }
        }
        let (_, mut state) = lane.payload.take().ok_or_else(|| error("Missing probe"))?;
        assert!(state.tick(&target, &mut SlowRead)?.is_none());
        assert!(entered.try_recv().is_err());
        assert!(changes.try_recv().is_err());
        Ok(())
    }
    #[test]
    fn polling_deadline_keeps_phase_and_skips_missed_slots() {
        let base = Instant::now();
        let period = Duration::from_secs(30);
        assert_eq!(
            next_deadline(base, base + Duration::from_secs(20), period),
            base + period
        );
        assert_eq!(
            next_deadline(base, base + Duration::from_secs(95), period),
            base + Duration::from_secs(120)
        );
    }
}
