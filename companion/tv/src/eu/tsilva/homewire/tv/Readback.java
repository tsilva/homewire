package eu.tsilva.homewire.tv;

import android.app.NotificationManager;
import android.content.ComponentName;
import android.content.Context;
import android.media.MediaMetadata;
import android.media.session.MediaController;
import android.media.session.MediaSessionManager;
import android.media.session.PlaybackState;
import android.os.SystemClock;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONObject;

final class Readback {
    static JSONObject snapshot(Context context, String foreground) throws Exception {
        long now = SystemClock.elapsedRealtime();
        ComponentName listener = new ComponentName(context, PlaybackAccessService.class);
        NotificationManager notifications = context.getSystemService(NotificationManager.class);
        boolean access = notifications.isNotificationListenerAccessGranted(listener);
        JSONArray sessions = new JSONArray();
        boolean truncated = false;
        if (access) {
            MediaSessionManager manager = context.getSystemService(MediaSessionManager.class);
            List<MediaController> active = manager.getActiveSessions(listener);
            for (MediaController controller : active) {
                if (sessions.length() == 4) { truncated = true; break; }
                PlaybackState state = controller.getPlaybackState();
                MediaMetadata metadata = controller.getMetadata();
                long updated = state == null ? 0 : state.getLastPositionUpdateTime();
                JSONObject session = new JSONObject().put("package", bounded(controller.getPackageName(), 256))
                    .put("state", state == null ? JSONObject.NULL : state.getState())
                    .put("position_ms", state == null ? JSONObject.NULL : state.getPosition())
                    .put("speed", state == null ? JSONObject.NULL : state.getPlaybackSpeed())
                    .put("updated_age_ms", updated > 0 && now >= updated ? now - updated : JSONObject.NULL)
                    .put("title", metadata == null ? JSONObject.NULL : nullable(metadata.getText(MediaMetadata.METADATA_KEY_TITLE)))
                    .put("display_title", metadata == null ? JSONObject.NULL : nullable(metadata.getText(MediaMetadata.METADATA_KEY_DISPLAY_TITLE)))
                    .put("media_id", metadata == null ? JSONObject.NULL : nullable(metadata.getText(MediaMetadata.METADATA_KEY_MEDIA_ID)))
                    .put("duration_ms", metadata != null && metadata.containsKey(MediaMetadata.METADATA_KEY_DURATION)
                        ? metadata.getLong(MediaMetadata.METADATA_KEY_DURATION) : JSONObject.NULL);
                sessions.put(session);
            }
        }
        return new JSONObject().put("foreground_package", foreground.isEmpty() ? JSONObject.NULL : foreground)
            .put("playback", new JSONObject().put("access_enabled", access).put("truncated", truncated).put("sessions", sessions));
    }
    private static Object nullable(CharSequence value) {
        String text = bounded(value, 256);
        return text.isEmpty() ? JSONObject.NULL : text;
    }
    private static String bounded(CharSequence value, int limit) {
        if (value == null) return "";
        StringBuilder result = new StringBuilder();
        for (int i = 0; i < value.length() && result.length() < limit; i++) {
            char c = value.charAt(i);
            if (!Character.isISOControl(c)) result.append(c);
        }
        if (result.length() > 0 && Character.isHighSurrogate(result.charAt(result.length() - 1))) {
            result.setLength(result.length() - 1);
        }
        return result.toString();
    }

}
