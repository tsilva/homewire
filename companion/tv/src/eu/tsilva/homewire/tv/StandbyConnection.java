package eu.tsilva.homewire.tv;

import android.content.Context;
import android.content.Intent;

/** Explicit, persistent opt-in; never changes microphone or screen settings. */
final class StandbyConnection {
    static boolean enabled(Context context) {
        return context.getSharedPreferences("lifecycle", 0)
            .getBoolean("keep_network_in_standby", false);
    }
    static void set(Context context, boolean enabled) throws Exception {
        Config.load(context).verify();
        if (!context.getSharedPreferences("lifecycle", 0).edit()
                .putBoolean("keep_network_in_standby", enabled).commit()) {
            throw new IllegalStateException("Standby preference was not saved");
        }
        if (context.getSharedPreferences("lifecycle", 0).getBoolean("enabled", true)) {
            context.startForegroundService(new Intent(context, TvService.class));
        }
    }
}
