//! Bounded, scoped routine workers. Only independent device chains overlap.
use crate::{
    Result,
    device::{Adapter, Devices, Family},
    entity::{Command, Device},
    error,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Mutex;

/// Spawn all workers before joining; preserve configuration order in replies.
/// Thread creation failure or an unwinding panic is incomplete, never a retry.
pub(super) fn run<T: Sync>(
    steps: &[T],
    control: &(impl Fn(&T) -> Result<Value> + Sync),
) -> Vec<Result<Value>> {
    if steps.len() > 16 {
        return steps
            .iter()
            .map(|_| Err(error("At most 16 independent routine workers are allowed")))
            .collect();
    }
    std::thread::scope(|scope| {
        let jobs = steps
            .iter()
            .map(|step| {
                std::thread::Builder::new()
                    .name("homewire-routine".into())
                    .stack_size(512 * 1024)
                    .spawn_scoped(scope, move || control(step))
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| match job {
                Ok(handle) => handle.join().unwrap_or_else(|_| {
                    Err(error(
                        "Routine worker failed; outcome unknown; do not replay",
                    ))
                }),
                Err(_) => Err(error("Routine worker unavailable; command not sent")),
            })
            .collect()
    })
}

pub(super) fn report<'a, T: Serialize + 'a>(
    steps: impl Iterator<Item = &'a T>,
    results: Vec<Result<Value>>,
) -> Value {
    let mut completed = true;
    let results = steps
        .zip(results)
        .map(|(step, result)| match result {
            Ok(result) => {
                let verified = result["completed"] != false && result["verified"] != false;
                completed &= verified;
                json!({"step":step,"completed":verified,"result":result})
            }
            Err(e) => {
                completed = false;
                json!({"step":step,"completed":false,"error":e.to_string().chars().take(512).collect::<String>()})
            }
        })
        .collect::<Vec<_>>();
    json!({"completed":completed,"verified":completed,"execution":"concurrent_best_effort","steps":results})
}

struct Tv {
    routine: Option<super::tcl::Manager>,
    adapter: Option<crate::androidtv::TclManager>,
}

/// Temporarily split stateful lanes from the shared, read-only device catalog.
/// Locks are per lane, never around the whole sequence or shared plug adapter.
/// Drop restores every manager, including poisoned lanes after a worker panic.
pub(super) struct Equipment<'a> {
    devices: &'a mut Devices,
    computer: Mutex<Option<super::computer::Manager>>,
    tv: Mutex<Tv>,
    meaco: Mutex<Option<crate::meaco::Manager>>,
    alerts: Mutex<Option<crate::alerts::Manager>>,
}

impl<'a> Equipment<'a> {
    /// The caller must validate the complete list and resolve power targets
    /// before splitting: temporarily removed managers carry ownership metadata.
    pub(super) fn new(devices: &'a mut Devices) -> Self {
        let computer = devices.computer_routine.take();
        let tv = Tv {
            routine: devices.tcl_routine.take(),
            adapter: devices.tcl.take(),
        };
        let meaco = devices.meaco.take();
        let alerts = devices.alerts.take();
        Self {
            devices,
            computer: Mutex::new(computer),
            tv: Mutex::new(tv),
            meaco: Mutex::new(meaco),
            alerts: Mutex::new(alerts),
        }
    }

    pub(super) fn devices(&self) -> &Devices {
        self.devices
    }

    pub(super) fn routine(&self, name: &str) -> Result<Value> {
        let result = match name {
            "computer_on" | "computer_off" => self
                .computer
                .lock()
                .map_err(|_| error("Computer lane failed; do not replay"))?
                .as_mut()
                .ok_or_else(|| error("Missing computer routine"))?
                .run_with(
                    self.devices
                        .plugs_ref()
                        .ok_or_else(|| error("Missing plugs"))?,
                    self.devices
                        .computer
                        .as_ref()
                        .ok_or_else(|| error("Missing computer"))?,
                    name == "computer_on",
                )?,
            "tv_on" | "tv_off" => {
                let mut lane = self
                    .tv
                    .lock()
                    .map_err(|_| error("TV lane failed; do not replay"))?;
                let Tv { routine, adapter } = &mut *lane;
                routine
                    .as_mut()
                    .ok_or_else(|| error("Missing TV routine"))?
                    .run_with(
                        self.devices
                            .plugs_ref()
                            .ok_or_else(|| error("Missing plugs"))?,
                        adapter.as_mut().ok_or_else(|| error("Missing TCL"))?,
                        name == "tv_on",
                    )?
            }
            "meaco_day" => self
                .meaco
                .lock()
                .map_err(|_| error("Meaco lane failed; do not replay"))?
                .as_mut()
                .ok_or_else(|| error("Missing Meaco"))?
                .execute("meaco_apply", &json!({"operation":"day"}))?,
            _ => return Err(error("Unsupported independent child routine")),
        };
        Ok(json!({"routine":name,"result":result}))
    }

    pub(super) fn power_on(&self, device: Device, id: &str) -> Result<Value> {
        let result = self.power_on_inner(device, id);
        match self.alerts.lock() {
            Ok(mut alerts) => {
                if let Some(alerts) = alerts.as_mut() {
                    crate::entity::record_control_result(alerts, id, &result);
                }
            }
            Err(_) => eprintln!("Control alert ledger unavailable after worker failure"),
        }
        result
    }

    fn power_on_inner(&self, device: Device, id: &str) -> Result<Value> {
        let result = if device.family == Family::Meaco {
            self.meaco
                .lock()
                .map_err(|_| error("Meaco lane failed; do not replay"))?
                .as_mut()
                .ok_or_else(|| error("Missing Meaco"))?
                .control_device(
                    device.index,
                    Command::Configure,
                    &json!({"settings":{"power":true}}),
                )?
        } else if device.family == Family::Plugs {
            self.devices
                .plugs_ref()
                .ok_or_else(|| error("Missing plugs"))?
                .power_device(device.index, true)?
        } else {
            self.devices.power_device(device, true)?
        };
        Ok(json!({"entity":id,"result":result}))
    }
}

impl Drop for Equipment<'_> {
    fn drop(&mut self) {
        self.devices.computer_routine = self
            .computer
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let tv = self.tv.get_mut().unwrap_or_else(|e| e.into_inner());
        self.devices.tcl_routine = tv.routine.take();
        self.devices.tcl = tv.adapter.take();
        self.devices.meaco = self
            .meaco
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.devices.alerts = self
            .alerts
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

#[cfg(test)]
#[path = "../../tests/support/mod.rs"]
#[allow(dead_code)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn stalled_chain_does_not_block_siblings_and_results_keep_input_order() {
        let (release, receive) = mpsc::sync_channel(1);
        let receive = Mutex::new(receive);
        let results = run(&[0, 1, 2], &|step| match step {
            0 => {
                receive
                    .lock()
                    .expect("receiver")
                    .recv_timeout(Duration::from_secs(5))?;
                Ok(json!({"verified":true,"step":step}))
            }
            1 => {
                release.send(())?;
                Err(error("Synthetic uncertain control"))
            }
            _ => Ok(json!({"completed":false,"verified":false})),
        });
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        let reply = report([0, 1, 2].iter(), results);
        assert_eq!(reply["completed"], false);
        assert_eq!(reply["steps"][0]["step"], 0);
        assert_eq!(reply["steps"][0]["completed"], true);
        assert_eq!(reply["steps"][1]["step"], 1);
        assert_eq!(reply["steps"][2]["completed"], false);
    }

    #[test]
    fn panic_is_incomplete_and_each_step_is_attempted_once() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let results = run(&[0, 1], &|step| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_ne!(*step, 0, "Synthetic worker panic");
            Ok(json!({"verified":true}))
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(results[0].is_err());
        assert!(results[1].is_ok());
    }

    #[test]
    fn oversized_batch_sends_no_commands() {
        let results = run(&[0; 17], &|_| panic!("Must not dispatch"));
        assert_eq!(results.len(), 17);
        assert!(results.iter().all(Result::is_err));
    }

    #[test]
    fn stateful_lanes_are_restored_even_after_a_poisoned_worker() -> Result<()> {
        let t = test_support::Directory::new()?;
        t.certs()?;
        let path = t.config("home.json", &json!({"version":1,"adapters":{
            "plugs":{"plugs":[
                {"name":"pc_supply","model":"P100","host":"192.0.2.30","device_id":"syntheticpc","auth_hash":"22".repeat(32)},
                {"name":"tv_supply","model":"P100","host":"192.0.2.31","device_id":"synthetictv","auth_hash":"22".repeat(32)}]},
            "meaco":{"device_id":"synthetichumidity","host":"192.0.2.1","local_key":"0123456789abcdef","protocol_version":"3.3"},
            "computer":{"name":"workstation","address":"192.0.2.50:39817","server_public_key":"22".repeat(32),"signing_key":"11".repeat(32),"mac":"02:00:00:00:00:50","wake_address":"192.0.2.255:9"},
            "tcl":{"host":"192.0.2.40","model":"Smart TV","server_cert_sha256":"22".repeat(32),"client_cert":"cert.pem","client_key":"key.pem"}},
            "routines":{
                "computer":{"plug":"pc_supply","startup_seconds":5,"ready_seconds":120,"shutdown_policy":"leave_on"},
                "tv":{"target":"tcl","plug":"tv_supply","startup_seconds":5,"ready_seconds":120}}}))?;
        let mut devices = crate::bundle::Bundle::load(&path)?
            .expect("bundle")
            .open_devices(None)?;
        {
            let equipment = Equipment::new(&mut devices);
            assert!(equipment.devices().computer_routine.is_none());
            assert!(equipment.devices().tcl.is_none());
            let results = run(&[0], &|_| {
                let _lane = equipment.meaco.lock().expect("Meaco lane");
                panic!("Synthetic poisoned lane; no device I/O")
            });
            assert!(results[0].is_err());
        }
        assert!(devices.meaco.is_some());
        assert!(devices.computer_routine.is_some());
        assert!(devices.tcl_routine.is_some());
        assert!(devices.tcl.is_some());
        devices.validate()?;
        Ok(())
    }
}
