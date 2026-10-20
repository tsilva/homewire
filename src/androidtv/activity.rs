//! One-shot activity plans. No UI macros, arbitrary intents or retained app state.
use super::*;

pub(super) const NAMES: &[&str] = &[
    "home",
    "quick_settings",
    "settings",
    "inputs",
    "live_tv",
    "hdmi",
    "netflix",
    "youtube",
    "prime_video",
    "stremio",
];
pub(super) fn options() -> Value {
    json!({"no_parameter":["home","quick_settings","settings","inputs","live_tv","prime_video"],"quick_settings":{"key":"home","hold_ms":800},"hdmi":{"required":"input","minimum":1,"maximum":4},"netflix":{"optional":"content_id","format":"1–12 digits"},"youtube":{"optional_one_of":["query","content_id"],"content_id_format":"11 letters, digits, underscore or hyphen"},"stremio":{"optional":"query"},"query_max_utf8_bytes":256,"requires_power_on":true,"playback_verified":false,"shortcuts_require_firmware_validation":["quick_settings","settings","inputs","live_tv","hdmi"]})
}
enum Action {
    Key(u64),
    Hold(u64),
    Link(String),
}
struct Plan<'a> {
    name: &'a str,
    action: Action,
    packages: &'static [&'static str],
}
fn plan(value: &Value) -> Result<Plan<'_>> {
    if !crate::media_input::valid_activity(value) {
        return Err(error("Invalid bounded TV activity"));
    }
    let name = value["name"]
        .as_str()
        .ok_or_else(|| error("Expected activity name"))?;
    let query = value["query"].as_str();
    let id = value["content_id"].as_str();
    let input = value["input"].as_u64();
    let plain = value.as_object().is_some_and(|o| o.len() == 1);
    let (action, packages): (_, &'static [&'static str]) = match name {
        "home" if plain => (
            Action::Key(3),
            &[
                "com.google.android.apps.tv.launcherx",
                "com.google.android.tvlauncher",
            ],
        ),
        "settings" if plain => (Action::Key(176), &[]),
        "quick_settings" if plain => (Action::Hold(3), &[]),
        "inputs" if plain => (Action::Key(178), &[]),
        "live_tv" if plain => (Action::Key(170), &[]),
        "hdmi" if let Some(input) = input => (Action::Key(242 + input), &[]),
        "netflix" if plain => (Action::Link("netflix://".into()), &["com.netflix.ninja"]),
        "netflix"
            if let Some(id) = id
                && id.len() <= 12
                && id.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (
                Action::Link(format!("https://www.netflix.com/title/{}?source=30", id)),
                &["com.netflix.ninja"],
            )
        }
        "youtube"
            if plain
                || query.is_some()
                || id.is_some_and(|s| {
                    s.len() == 11
                        && s.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                }) =>
        {
            let url = if let Some(query) = query {
                format!(
                    "https://www.youtube.com/results?search_query={}",
                    crate::web::encode(query)
                )
            } else if let Some(id) = id {
                format!("https://www.youtube.com/watch?v={id}")
            } else {
                "https://www.youtube.com".into()
            };
            (Action::Link(url), &["com.google.android.youtube.tv"])
        }
        "prime_video" if plain => (
            Action::Link("https://app.primevideo.com/home".into()),
            &["com.amazon.amazonvideo.livingroom"],
        ),
        "stremio" if plain || query.is_some() => (
            Action::Link(query.map_or_else(
                || "stremio:///board".into(),
                |q| format!("stremio:///search?search={}", crate::web::encode(q)),
            )),
            &["com.stremio.one"],
        ),
        _ => {
            return Err(error(
                "Unsupported activity parameters; home/settings/inputs/live_tv/prime_video take no parameter; hdmi requires input 1–4; netflix accepts a numeric content_id; youtube accepts query or an 11-character content_id; stremio accepts query",
            ));
        }
    };
    if let Action::Link(link) = &action {
        validate_app_link(link)?;
    }
    Ok(Plan {
        name,
        action,
        packages,
    })
}
impl<const TCL: bool> RemoteManager<TCL> {
    pub(super) fn activity(&mut self, value: &Value) -> Result<Value> {
        let plan = plan(value)?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.prepare_control::<TCL>(deadline)?;
        if !remote.status.as_ref().is_some_and(|s| s.power) {
            return Err(error(
                "TV is not confirmed on; use media_power before an activity",
            ));
        }
        if matches!(plan.action, Action::Link(_)) && remote.features & 512 == 0 {
            return Err(error("Android TV does not support app links"));
        }
        if let Some(status) = remote.status.as_mut() {
            status.current_app = None;
        }
        match plan.action {
            Action::Key(code) => remote.key(code)?,
            Action::Hold(code) => remote.hold_key(code, 800, deadline)?,
            Action::Link(link) => {
                let mut body = Vec::new();
                pb::bytes(1, link.as_bytes(), &mut body);
                pb::write(&mut remote.stream, 90, &body)?;
            }
        }
        remote.finish_control(deadline, Duration::from_secs(2))?;
        let current_app = remote
            .status
            .as_ref()
            .and_then(|s| s.current_app.as_deref());
        let app_confirmed = current_app
            .filter(|_| !plan.packages.is_empty())
            .map(|p| plan.packages.contains(&p));
        Ok(
            json!({"sent":true,"verified":false,"activity":plan.name,"current_app":current_app,"app_confirmed":app_confirmed,"note":"Fresh app reports do not confirm playback, screen contents or an input switch; never replay automatically"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_are_bounded_and_send_only_one_action() {
        for name in NAMES.iter().filter(|n| **n != "hdmi") {
            assert!(plan(&json!({"name":name})).is_ok());
        }
        assert!(matches!(
            plan(&json!({"name":"hdmi","input":4}))
                .expect("valid activity")
                .action,
            Action::Key(246)
        ));
        for value in [
            json!({"name":"home","query":"x"}),
            json!({"name":"hdmi"}),
            json!({"name":"hdmi","input":5}),
            json!({"name":"netflix","content_id":"../x"}),
            json!({"name":"youtube","content_id":"short"}),
            json!({"name":"youtube","query":"x","content_id":"abcdefghijk"}),
            json!({"name":"stremio","query":"x\n"}),
            json!({"name":"sleep"}),
            json!({"name":"stremio","url":"stremio:///addons"}),
            json!({"name":"stremio","query":"x".repeat(257)}),
        ] {
            assert!(plan(&value).is_err(), "{value}");
        }
        let Action::Link(url) = plan(&json!({"name":"youtube","query":"a & b/é"}))
            .expect("valid activity")
            .action
        else {
            panic!()
        };
        assert_eq!(
            url,
            "https://www.youtube.com/results?search_query=a%20%26%20b%2F%C3%A9"
        );
        let Action::Link(url) = plan(&json!({"name":"netflix","content_id":"80057281"}))
            .expect("valid activity")
            .action
        else {
            panic!()
        };
        assert_eq!(url, "https://www.netflix.com/title/80057281?source=30");
    }
}
