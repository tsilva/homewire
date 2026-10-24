package eu.tsilva.homewire.tv;

import android.content.Context;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONArray;
import org.json.JSONObject;

/** Bounded durable replay history and one conservative pending control guard. */
final class Journal {
    private final Context context;
    private JSONObject state;
    Journal(Context context) throws Exception {
        this.context = context;
        if (Config.file(context, "controls.json").getBaseFile().exists()) {
            try (InputStream in = Config.file(context, "controls.json").openRead()) {
                state = new JSONObject(new String(Config.bounded(in, 4096), StandardCharsets.UTF_8));
            }
            JSONArray ids = state.getJSONArray("ids");
            if (ids.length() > 16) throw new Exception("Invalid replay journal");
            for (int i = 0; i < ids.length(); i++) {
                if (!ids.getString(i).matches("[a-f0-9]{32}")) throw new Exception("Invalid replay id");
            }
            if (state.has("pending")) {
                JSONObject pending = state.getJSONObject("pending");
                if (pending.getInt("level") < 0 || pending.getInt("level") > 100
                    || pending.getLong("at") < 0) throw new Exception("Invalid pending control");
            }
        } else state = new JSONObject().put("ids", new JSONArray());
    }
    void reconcile(int current) throws Exception {
        if (state.has("pending") && state.getJSONObject("pending").getInt("level") == current) {
            state.remove("pending"); save();
        }
    }
    void record(String id, int level, int current) throws Exception {
        JSONArray ids = state.getJSONArray("ids");
        for (int i = 0; i < ids.length(); i++) {
            if (id.equals(ids.getString(i))) throw new ApiServer.Failure(409, "request_replayed");
        }
        reconcile(current);
        if (state.has("pending") && System.currentTimeMillis() < state.getJSONObject("pending").getLong("at") + 120000) {
            throw new ApiServer.Failure(409, "previous_control_unconfirmed");
        }
        JSONArray next = new JSONArray();
        for (int i = Math.max(0, ids.length() - 15); i < ids.length(); i++) next.put(ids.getString(i));
        next.put(id);
        JSONObject updated = new JSONObject().put("ids", next);
        if (current != level) updated.put("pending", new JSONObject().put("level", level).put("at", System.currentTimeMillis()));
        // Persist before the caller can send any device-changing operation.
        Config.save(Config.file(context, "controls.json"), updated);
        state = updated;
    }
    private void save() throws Exception { Config.save(Config.file(context, "controls.json"), state); }
}
