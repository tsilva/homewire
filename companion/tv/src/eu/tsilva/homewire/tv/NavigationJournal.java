package eu.tsilva.homewire.tv;

import android.content.Context;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONArray;
import org.json.JSONObject;

/** Separate bounded durable history; persist attempts before any navigation. */
final class NavigationJournal {
    private final Context context;
    NavigationJournal(Context context) { this.context = context; }
    void record(String id) throws Exception {
        JSONArray ids = new JSONArray();
        if (Config.file(context, "navigation.json").getBaseFile().exists()) {
            try (InputStream in = Config.file(context, "navigation.json").openRead()) {
                ids = new JSONObject(new String(Config.bounded(in, 2048), StandardCharsets.UTF_8)).getJSONArray("ids");
            }
        }
        if (ids.length() > 16) throw new IllegalStateException("Invalid navigation journal");
        for (int i = 0; i < ids.length(); i++) {
            String previous = ids.getString(i);
            if (!previous.matches("[a-f0-9]{32}")) throw new IllegalStateException("Invalid navigation journal");
            if (id.equals(previous)) throw new ApiServer.Failure(409, "request_replayed");
        }
        JSONArray next = new JSONArray();
        for (int i = Math.max(0, ids.length() - 15); i < ids.length(); i++) next.put(ids.getString(i));
        next.put(id);
        Config.save(Config.file(context, "navigation.json"), new JSONObject().put("ids", next));
    }
}
