//! Battery warnings from authenticated companion measurements, without new snapshots.
use crate::{Result, config, error, fault, mobile::Report};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

const LOW: &str = "low_battery";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub devices: Vec<String>,
    #[serde(default = "low")]
    pub low_percent: u8,
    #[serde(default = "recovery")]
    pub recovery_percent: u8,
    #[serde(default = "poll")]
    pub poll_seconds: u64,
}
fn low() -> u8 {
    15
}
fn recovery() -> u8 {
    20
}
fn poll() -> u64 {
    60
}
#[derive(Serialize)]
pub struct Unavailable {
    device: String,
    error: String,
}
struct Status {
    percent: u8,
    received_at: i64,
}
struct Endpoint<'a> {
    name: &'a str,
    report: Option<&'a Report>,
    now: i64,
    max_age: i64,
    config: &'a Config,
    seen: Option<(i64, u8)>,
}
fn is_low(percent: u8, published: bool, config: &Config) -> bool {
    if published {
        percent < config.recovery_percent
    } else {
        percent < config.low_percent
    }
}
impl fault::Target for Endpoint<'_> {
    type Status = Status;
    type Unavailable = Unavailable;
    type Event = fault::Event;
    fn name(&self) -> &str {
        self.name
    }
    fn identity(&self) -> String {
        format!(
            "companion:{}:{}:{}",
            self.name, self.config.low_percent, self.config.recovery_percent
        )
    }
    fn read(&self, _: Duration) -> Result<Status> {
        let report = self
            .report
            .filter(|r| {
                r.received_at > 0
                    && self.now >= r.received_at
                    && self.now - r.received_at <= self.max_age
            })
            .ok_or_else(|| error("Battery telemetry is missing or stale"))?;
        Ok(Status {
            percent: report.battery_percent,
            received_at: report.received_at,
        })
    }
    fn faults(_: &Status) -> fault::Faults<'_> {
        fault::Faults::Unknown
    }
    fn monitored_faults<'a>(&self, status: &'a Status, published: &[String]) -> fault::Faults<'a> {
        if self.seen == Some((status.received_at, status.percent)) {
            return fault::Faults::NoSample;
        }
        fault::Faults::flag(
            Some(is_low(
                status.percent,
                published.iter().any(|f| f == LOW),
                self.config,
            )),
            LOW,
        )
    }
    fn unavailable(&self, error: String) -> Unavailable {
        Unavailable {
            device: self.name.to_owned(),
            error,
        }
    }
}

pub(crate) struct Monitor {
    config: Config,
    monitor: fault::Monitor,
    store: fault::Store,
    next_poll: Instant,
    seen: [Option<(i64, u8)>; 8],
}
impl Monitor {
    pub(crate) fn open(path: &Path, config: Config, devices: &[String]) -> Result<Self> {
        if config.devices.is_empty()
            || config.devices.len() > 8
            || !config::valid_unique_labels(config.devices.iter().map(String::as_str), 32, b"-_")
            || config.devices.iter().any(|name| !devices.contains(name))
            || !(15..=3600).contains(&config.poll_seconds)
            || config.low_percent == 0
            || config.low_percent >= config.recovery_percent
            || config.recovery_percent > 100
        {
            return Err(error("Invalid companion battery monitoring configuration"));
        }
        let targets: Vec<_> = config
            .devices
            .iter()
            .map(|name| Endpoint {
                name,
                report: None,
                now: 0,
                max_age: 0,
                config: &config,
                seen: None,
            })
            .collect();
        let (store, monitor) = fault::Store::open(&path.with_extension("battery.json"), &targets)?;
        Ok(Self {
            config,
            monitor,
            store,
            next_poll: Instant::now(),
            seen: [None; 8],
        })
    }
    pub(crate) fn poll_seconds(&self) -> u64 {
        self.config.poll_seconds
    }
    pub(crate) fn next_poll(&self) -> Instant {
        self.next_poll
    }
    pub(crate) fn report_received(&mut self, name: &str) {
        if self.config.devices.iter().any(|device| device == name) {
            self.next_poll = Instant::now();
        }
    }
    pub(crate) fn watch(
        &mut self,
        reports: &BTreeMap<String, Report>,
        max_age: i64,
        now: i64,
    ) -> Result<fault::Watch<Unavailable>> {
        self.next_poll = Instant::now() + Duration::from_secs(self.config.poll_seconds);
        let targets: Vec<_> = self
            .config
            .devices
            .iter()
            .enumerate()
            .map(|(i, name)| Endpoint {
                name,
                report: reports.get(name),
                now,
                max_age,
                config: &self.config,
                seen: self.seen[i],
            })
            .collect();
        let result = self
            .store
            .watch(&mut self.monitor, &targets, Duration::ZERO);
        if result.is_ok() {
            for (i, target) in targets.iter().enumerate() {
                if let Some(report) = target.report {
                    self.seen[i] = Some((report.received_at, report.battery_percent));
                }
            }
        }
        result
    }
    pub(crate) fn background_tick(
        &mut self,
        reports: &BTreeMap<String, Report>,
        max_age: i64,
    ) -> Result<()> {
        let check = self.watch(reports, max_age, chrono::Utc::now().timestamp())?;
        for event in check.events {
            eprintln!("Battery {}: {}", event.device, event.event);
        }
        for unavailable in check.unavailable {
            eprintln!("Battery {}: {}", unavailable.device, unavailable.error);
        }
        Ok(())
    }
    pub(crate) fn active_faults(&self, visit: &mut impl FnMut(&str, &[String])) {
        for (name, saved) in &self.monitor.memory.devices {
            visit(name, &saved.faults);
        }
    }
    pub(crate) fn reconcile_alerts(
        &self,
        alerts: &mut crate::alerts::Manager,
        now: i64,
    ) -> Result<()> {
        for name in &self.config.devices {
            if let Some(saved) = self.monitor.memory.devices.get(name) {
                let active = saved.faults.iter().any(|f| f == LOW);
                let message = if active {
                    format!(
                        "Battery is below {}%. Please check its power supply.",
                        self.config.low_percent
                    )
                } else {
                    format!(
                        "Battery has recovered to at least {}%.",
                        self.config.recovery_percent
                    )
                };
                alerts.record(
                    &format!("battery.{name}"),
                    active,
                    &format!("{name}: low battery"),
                    &message,
                    now,
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fault::Target;
    #[test]
    fn battery_thresholds_and_report_freshness_are_exact() {
        let config = Config {
            devices: vec!["phone".into()],
            low_percent: 15,
            recovery_percent: 20,
            poll_seconds: 60,
        };
        for percent in 0..=100 {
            assert_eq!(is_low(percent, false, &config), percent < 15);
            assert_eq!(is_low(percent, true, &config), percent < 20);
        }
        let report = Report {
            battery_percent: 14,
            battery_state: "charging".into(),
            charger: "usb".into(),
            location: None,
            received_at: 100,
        };
        let mut endpoint = Endpoint {
            name: "phone",
            report: Some(&report),
            now: 160,
            max_age: 60,
            config: &config,
            seen: None,
        };
        let status = endpoint.read(Duration::ZERO).expect("fresh boundary");
        assert!(matches!(
            endpoint.monitored_faults(&status, &[]),
            fault::Faults::One(LOW)
        ));
        endpoint.seen = Some((100, 14));
        assert!(matches!(
            endpoint.monitored_faults(&status, &[]),
            fault::Faults::NoSample
        ));
        endpoint.now = 161;
        assert!(endpoint.read(Duration::ZERO).is_err());
        endpoint.now = 99;
        assert!(endpoint.read(Duration::ZERO).is_err());
        endpoint.report = None;
        assert!(endpoint.read(Duration::ZERO).is_err());
    }
}
