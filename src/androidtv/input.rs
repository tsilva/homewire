//! Android remote keys, bounded holds and PCM voice sessions.
use super::*;

macro_rules! keys {
    ($($name:literal => $code:literal),+ $(,)?) => {
        pub(super) const KEYS: &[&str] = &[$($name),+];
        pub(super) fn key_code(key: &str) -> Option<u64> {
            match key { $($name => Some($code),)+ _ => None }
        }
    };
}
keys! {
    "home" => 3, "back" => 4,
    "up" => 19, "down" => 20, "left" => 21, "right" => 22, "enter" => 23,
    "volume_up" => 24, "volume_down" => 25, "mute" => 164,
    "play_pause" => 85, "play" => 126, "pause" => 127,
    "stop" => 86, "next" => 87, "previous" => 88, "rewind" => 89, "fast_forward" => 90,
    "skip_forward" => 272, "skip_backward" => 273,
    "channel_up" => 166, "channel_down" => 167,
    "0" => 7, "1" => 8, "2" => 9, "3" => 10, "4" => 11,
    "5" => 12, "6" => 13, "7" => 14, "8" => 15, "9" => 16,
    "delete" => 67, "menu" => 82, "search" => 84,
    "info" => 165, "tv" => 170, "guide" => 172, "captions" => 175,
    "settings" => 176, "input" => 178,
    "red" => 183, "green" => 184, "yellow" => 185, "blue" => 186,
    "app_switch" => 187, "assist" => 219, "audio_track" => 222,
    "last_channel" => 229, "teletext" => 233,
    "hdmi_1" => 243, "hdmi_2" => 244, "hdmi_3" => 245, "hdmi_4" => 246
}

impl<const TCL: bool> RemoteManager<TCL> {
    pub(super) fn hold(&mut self, args: &Value) -> Result<Value> {
        if !crate::media_input::valid_hold(args) {
            return Err(error("Expected entity, key and hold_ms (1–2000)"));
        }
        let key = args["key"]
            .as_str()
            .ok_or_else(|| error("Expected remote key"))?;
        let code = key_code(key).ok_or_else(|| error("Unsupported Android TV key"))?;
        let hold_ms = args["hold_ms"]
            .as_u64()
            .ok_or_else(|| error("Expected hold_ms"))?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote::<TCL>(deadline)?;
        remote.prepare_control::<TCL>(deadline)?;
        remote.hold_key(code, hold_ms, deadline)?;
        remote.finish_key(deadline)?;
        Ok(json!({"sent":true,"verified":false,"key":key,"hold_ms":hold_ms}))
    }
    pub(super) fn voice(&mut self, audio: &Value) -> Result<Value> {
        let audio = crate::media_input::decode_voice(audio)?;
        let deadline = Instant::now() + Duration::from_secs(self.config.timeout_seconds);
        let mut remote = self.config.remote_input::<TCL>(deadline, true)?;
        remote.prepare_control::<TCL>(deadline)?;
        if remote.features & 8 == 0 {
            return Err(error("Android TV does not support voice input"));
        }
        if remote.voice_session.is_some() {
            return Err(error("Unexpected voice session before search request"));
        }
        let ready = deadline.min(Instant::now() + Duration::from_secs(2));
        remote.stream.sock.set_deadline(ready);
        remote.key(84)?;
        if remote.until(|r| r.voice_session.is_some()).is_err() {
            return Err(error(
                "Voice search requested once but session was not confirmed; do not replay",
            ));
        }
        remote.stream.sock.set_deadline(deadline);
        let session = remote
            .voice_session
            .ok_or_else(|| error("Missing voice session"))?;
        let mut begin = Vec::new();
        pb::integer(1, session, &mut begin);
        // Once a session id is known, always attempt its end on the same session.
        let sent = (|| -> Result<()> {
            pb::write(&mut remote.stream, 30, &begin)?;
            for samples in audio.chunks(8192) {
                let mut payload = Vec::with_capacity(8224);
                pb::integer(1, session, &mut payload);
                if samples.len() == 8192 {
                    pb::bytes(2, samples, &mut payload);
                } else {
                    let mut padded = [0; 8192];
                    padded[..samples.len()].copy_from_slice(samples);
                    pb::bytes(2, &padded, &mut payload);
                }
                pb::write_limit(&mut remote.stream, 31, &payload, 8256)?;
            }
            Ok(())
        })();
        let ended = pb::write(&mut remote.stream, 32, &begin);
        if sent.is_err() || ended.is_err() {
            return Err(error(
                "Voice stream failed; end attempted once; outcome unknown; do not replay",
            ));
        }
        remote.finish_key(deadline)?;
        Ok(
            json!({"sent":true,"verified":false,"audio_ms":audio.len()/16,"note":"Voice recognition and assistant actions are unverified; never replay automatically"}),
        )
    }
}

impl Remote<'_> {
    pub(super) fn hold_key(&mut self, code: u64, hold_ms: u64, deadline: Instant) -> Result<()> {
        let release_at = Instant::now() + Duration::from_millis(hold_ms);
        if release_at + Duration::from_millis(600) >= deadline {
            return Err(error(
                "Not enough operation time remaining to hold and release key",
            ));
        }
        self.key_direction(code, 1)?;
        let held = self.wait_held(release_at);
        // Release once even after a read failure; never reconnect or replay.
        self.stream.sock.set_deadline(deadline);
        let released = self.key_direction(code, 2);
        if held.is_err() || released.is_err() {
            return Err(error(
                "Remote hold failed; release attempted once; outcome unknown; do not replay",
            ));
        }
        Ok(())
    }
    fn wait_held(&mut self, deadline: Instant) -> Result<()> {
        self.stream.sock.set_deadline(deadline);
        for _ in 0..128 {
            if Instant::now() >= deadline {
                return Ok(());
            }
            if let Err(failure) = self.receive() {
                if failure.downcast_ref::<std::io::Error>().is_some_and(|e| {
                    matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    )
                }) {
                    return Ok(());
                }
                return Err(failure);
            }
        }
        Err(error("Too many Android remote messages during held key"))
    }
}
