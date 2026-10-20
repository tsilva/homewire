use homewire::{
    Result,
    config::Memory,
    error,
    meaco::{Action, Device, Operation, State, apply as apply_configured, plan as plan_configured},
};
use serde_json::json;

fn presets() -> homewire::routine::meaco::Presets {
    serde_json::from_value(json!({"day":{"mode":"manual","target_humidity":50},"night":{"mode":"purify","sleep":true}})).expect("fixture")
}
fn apply(
    device: &mut Fake,
    operation: Operation,
    memory: &mut Memory,
    persist: impl FnMut(&Memory) -> Result<()>,
) -> Result<homewire::meaco::Applied> {
    apply_configured(device, operation, memory, &presets(), persist)
}
fn plan(state: &State, operation: Operation, memory: &Memory) -> Vec<Action> {
    plan_configured(state, operation, memory, &presets()).expect("fixture")
}
struct Fake {
    state: State,
    base: String,
    commands: Vec<Action>,
    stuck_sleep: bool,
    lose_power: bool,
    reject_humidity: bool,
    uncertain: bool,
}
impl Fake {
    fn new(mode: &str, power: bool) -> Self {
        Self {
            state: State {
                power,
                mode: mode.into(),
                target_humidity: 45,
                current_humidity: Some(61),
                fault_code: 0,
                tank_full: false,
                defrost: false,
                problem: false,
            },
            base: "manual".into(),
            commands: vec![],
            stuck_sleep: false,
            lose_power: false,
            reject_humidity: false,
            uncertain: false,
        }
    }
}
impl Device for Fake {
    fn read(&mut self) -> Result<State> {
        Ok(self.state.clone())
    }
    fn settle(&mut self) {}
    fn command(&mut self, action: &Action) -> Result<()> {
        self.commands.push(action.clone());
        if self.uncertain {
            return Err(error("Outcome unknown"));
        }
        match action {
            Action::Power(p) => self.state.power = *p,
            Action::Humidity(h) if !self.reject_humidity => self.state.target_humidity = *h,
            Action::Mode(m) if m == "sleep" && !self.stuck_sleep => {
                if self.state.mode == "sleep" {
                    self.state.mode = self.base.clone();
                } else {
                    self.base = self.state.mode.clone();
                    self.state.mode = "sleep".into();
                }
            }
            Action::Mode(m) if m == "sleep" => {}
            Action::Mode(m) => self.state.mode = m.clone(),
            _ => {}
        }
        if self.lose_power {
            self.state.power = false;
        }
        Ok(())
    }
}
#[test]
fn day_turns_on_clears_sleep_and_verifies_55_percent() -> Result<()> {
    let mut fake = Fake::new("sleep", false);
    let mut memory = Memory::default();
    let result = apply(&mut fake, Operation::Day, &mut memory, |_| Ok(()))?;
    assert_eq!(
        fake.commands,
        vec![
            Action::Power(true),
            Action::Mode("sleep".into()),
            Action::Humidity(50)
        ]
    );
    assert_eq!(result.state.mode, "manual");
    assert_eq!(memory.base_mode.as_deref(), Some("manual"));
    Ok(())
}
#[test]
fn night_sets_purify_then_toggles_sleep_once() -> Result<()> {
    let mut fake = Fake::new("manual", true);
    let mut memory = Memory::default();
    apply(&mut fake, Operation::Night, &mut memory, |_| Ok(()))?;
    assert_eq!(
        fake.commands,
        vec![Action::Mode("purify".into()), Action::Mode("sleep".into())]
    );
    assert_eq!(memory.base_mode.as_deref(), Some("purify"));
    let again = apply(&mut fake, Operation::Night, &mut memory, |_| Ok(()))?;
    assert!(!again.changed);
    assert_eq!(fake.commands.len(), 2);
    Ok(())
}
#[test]
fn restart_with_unknown_base_reestablishes_purify() -> Result<()> {
    let mut fake = Fake::new("sleep", true);
    let mut memory = Memory::default();
    apply(&mut fake, Operation::Night, &mut memory, |_| Ok(()))?;
    assert_eq!(
        fake.commands,
        vec![
            Action::Mode("sleep".into()),
            Action::Mode("purify".into()),
            Action::Mode("sleep".into())
        ]
    );
    Ok(())
}
#[test]
fn sleep_verification_failure_never_retries_toggle() {
    let mut fake = Fake::new("sleep", true);
    fake.stuck_sleep = true;
    let mut memory = Memory::default();
    assert!(apply(&mut fake, Operation::Day, &mut memory, |_| Ok(())).is_err());
    assert_eq!(fake.commands, vec![Action::Mode("sleep".into())]);
    assert_eq!(memory.base_mode, None);
}
#[test]
fn uncertain_command_never_retries() {
    let mut fake = Fake::new("manual", true);
    fake.uncertain = true;
    assert!(
        apply(&mut fake, Operation::Night, &mut Memory::default(), |_| Ok(
            ()
        ))
        .is_err()
    );
    assert_eq!(fake.commands.len(), 1);
}
#[test]
fn humidity_verification_stops_before_changing_mode() {
    let mut fake = Fake::new("purify", true);
    fake.reject_humidity = true;
    assert!(
        apply(
            &mut fake,
            Operation::Day,
            &mut Memory::default(),
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(fake.commands, vec![Action::Humidity(50)]);
}
#[test]
fn power_loss_stops_before_sleep() {
    let mut fake = Fake::new("manual", true);
    fake.lose_power = true;
    assert!(
        apply(&mut fake, Operation::Night, &mut Memory::default(), |_| Ok(
            ()
        ))
        .is_err()
    );
    assert_eq!(fake.commands.len(), 1);
}
#[test]
fn persistence_failure_sends_no_commands() {
    let mut fake = Fake::new("manual", true);
    assert!(
        apply(
            &mut fake,
            Operation::Night,
            &mut Memory::default(),
            |_| Err(error("Disk full"))
        )
        .is_err()
    );
    assert!(fake.commands.is_empty());
}
#[test]
fn already_correct_day_is_idempotent() -> Result<()> {
    let mut fake = Fake::new("manual", true);
    fake.state.target_humidity = 50;
    assert!(
        !apply(
            &mut fake,
            Operation::Day,
            &mut Memory::default(),
            |_| Ok(())
        )?
        .changed
    );
    assert!(fake.commands.is_empty());
    Ok(())
}
#[test]
fn off_is_verified_and_repeated_off_is_a_noop() -> Result<()> {
    let mut fake = Fake::new("manual", true);
    let mut memory = Memory::default();
    assert!(
        !apply(&mut fake, Operation::Off, &mut memory, |_| Ok(()))?
            .state
            .power
    );
    assert!(!apply(&mut fake, Operation::Off, &mut memory, |_| Ok(()))?.changed);
    assert_eq!(fake.commands, vec![Action::Power(false)]);
    Ok(())
}
#[test]
fn planning_sends_no_commands() {
    let fake = Fake::new("manual", true);
    assert_eq!(
        plan(&fake.state, Operation::Night, &Memory::default()).len(),
        2
    );
    assert!(fake.commands.is_empty());
}
#[test]
fn unavailable_or_unknown_data_is_not_a_clear_fault() {
    assert!(
        State::from_dps(
            json!({"1":true,"2":50,"4":"manual"})
                .as_object()
                .expect("object")
        )
        .is_err()
    );
    assert!(
        State::from_dps(
            json!({"1":true,"2":50,"4":"unsupported","19":0})
                .as_object()
                .expect("object")
        )
        .is_err()
    );
}
#[test]
fn fault_mapping_matches_the_existing_device_mapping() -> Result<()> {
    for (code, tank, defrost, problem) in [
        (0, false, false, false),
        (1, true, false, false),
        (2, false, true, false),
        (128, false, false, false),
        (4, false, false, true),
    ] {
        let state = State::from_dps(
            json!({"1":true,"2":50,"4":"manual","19":code})
                .as_object()
                .expect("object"),
        )?;
        assert_eq!(
            (state.tank_full, state.defrost, state.problem),
            (tank, defrost, problem)
        );
    }
    Ok(())
}

#[test]
fn custom_presets_control_both_plan_and_verification() -> Result<()> {
    let p: homewire::routine::meaco::Presets = serde_json::from_value(
        json!({"day":{"mode":"manual","target_humidity":60},"night":{"mode":"manual","sleep":true}}),
    )?;
    let mut fake = Fake::new("purify", true);
    let mut memory = Memory::default();
    let expected = plan_configured(&fake.state, Operation::Day, &memory, &p)?;
    let applied = apply_configured(&mut fake, Operation::Day, &mut memory, &p, |_| Ok(()))?;
    assert_eq!(fake.commands, expected);
    assert_eq!(applied.state.target_humidity, 60);
    fake.commands.clear();
    apply_configured(&mut fake, Operation::Night, &mut memory, &p, |_| Ok(()))?;
    assert_eq!(fake.commands, vec![Action::Mode("sleep".into())]);
    assert_eq!(memory.base_mode.as_deref(), Some("manual"));
    assert!(!apply_configured(&mut fake, Operation::Night, &mut memory, &p, |_| Ok(()))?.changed);
    Ok(())
}
#[test]
fn missing_presets_and_invalid_targets_send_no_controls() {
    let mut fake = Fake::new("manual", true);
    let mut memory = Memory::default();
    for p in [
        homewire::routine::meaco::Presets::default(),
        serde_json::from_value(json!({"day":{"mode":"manual","target_humidity":56}}))
            .expect("deserialized"),
    ] {
        assert!(apply_configured(&mut fake, Operation::Day, &mut memory, &p, |_| Ok(())).is_err());
        assert!(fake.commands.is_empty());
    }
}

#[test]
fn independent_modes_preserve_the_other_mode_power_and_target() -> Result<()> {
    use homewire::routine::meaco::set_modes;
    let mut fake = Fake::new("manual", true);
    let mut memory = Memory::default();
    let purifier = set_modes(&mut fake, Some(true), None, &mut memory, |_| Ok(()))?;
    assert!(purifier.verified && purifier.changed);
    assert_eq!(purifier.purification, Some(true));
    assert!(!purifier.sleep);
    let asleep = set_modes(&mut fake, None, Some(true), &mut memory, |_| Ok(()))?;
    assert!(asleep.sleep);
    assert_eq!(asleep.purification, Some(true));
    // An identical sleep request must never send another toggle.
    let commands = fake.commands.len();
    assert!(!set_modes(&mut fake, None, Some(true), &mut memory, |_| Ok(()))?.changed);
    assert_eq!(fake.commands.len(), commands);
    // Turning purification off during sleep verifies the real underlying mode,
    // establishes normal dehumidification, and restores sleep.
    let result = set_modes(&mut fake, Some(false), None, &mut memory, |_| Ok(()))?;
    assert!(result.sleep);
    assert_eq!(result.purification, Some(false));
    assert_eq!(memory.base_mode.as_deref(), Some("manual"));
    let awake = set_modes(&mut fake, None, Some(false), &mut memory, |_| Ok(()))?;
    assert_eq!(awake.state.mode, "manual");
    assert!(awake.state.power);
    assert_eq!(awake.state.target_humidity, 45);
    assert!(fake.commands.iter().all(|a| matches!(a, Action::Mode(_))));
    Ok(())
}

#[test]
fn independent_modes_preserve_purification_when_clearing_sleep() -> Result<()> {
    let mut fake = Fake::new("sleep", true);
    fake.base = "purify".into();
    let mut memory = Memory {
        base_mode: Some("manual".into()),
        ..Memory::default()
    };
    let result =
        homewire::routine::meaco::set_modes(&mut fake, None, Some(false), &mut memory, |_| Ok(()))?;
    assert_eq!(result.state.mode, "purify");
    assert_eq!(memory.base_mode.as_deref(), Some("purify"));
    assert_eq!(fake.commands, vec![Action::Mode("sleep".into())]);
    Ok(())
}

#[test]
fn independent_modes_stop_after_one_uncertain_or_stuck_sleep_toggle() {
    for uncertain in [true, false] {
        let mut fake = Fake::new("sleep", true);
        fake.uncertain = uncertain;
        fake.stuck_sleep = !uncertain;
        let mut memory = Memory {
            base_mode: Some("purify".into()),
            ..Memory::default()
        };
        assert!(
            homewire::routine::meaco::set_modes(
                &mut fake,
                Some(false),
                Some(false),
                &mut memory,
                |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(fake.commands, vec![Action::Mode("sleep".into())]);
        assert_eq!(memory.base_mode, None);
    }
}

#[test]
fn independent_modes_refuse_controls_if_invalidation_cannot_persist() {
    let mut fake = Fake::new("manual", true);
    let mut memory = Memory::default();
    assert!(
        homewire::routine::meaco::set_modes(&mut fake, None, Some(true), &mut memory, |_| Err(
            error("disk full")
        ))
        .is_err()
    );
    assert!(fake.commands.is_empty());
}

#[test]
fn independent_modes_never_power_on_or_continue_after_unexpected_power_loss() -> Result<()> {
    let mut fake = Fake::new("manual", false);
    let mut memory = Memory::default();
    let result =
        homewire::routine::meaco::set_modes(&mut fake, Some(true), None, &mut memory, |_| Ok(()))?;
    assert!(!result.state.power);
    fake.state.power = true;
    fake.lose_power = true;
    assert!(
        homewire::routine::meaco::set_modes(
            &mut fake,
            Some(false),
            Some(true),
            &mut memory,
            |_| Ok(())
        )
        .is_err()
    );
    assert_eq!(fake.commands.len(), 2);
    assert_eq!(memory.base_mode, None);
    Ok(())
}
