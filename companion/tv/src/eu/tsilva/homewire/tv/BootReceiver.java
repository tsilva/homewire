package eu.tsilva.homewire.tv;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public final class BootReceiver extends BroadcastReceiver {
    @Override public void onReceive(Context context, Intent intent) {
        if (!Intent.ACTION_BOOT_COMPLETED.equals(intent.getAction())) return;
        if (!context.getSharedPreferences("lifecycle", 0).getBoolean("enabled", true)) return;
        try { Config.load(context); context.startForegroundService(new Intent(context, TvService.class)); }
        catch (Exception ignored) { }
    }
}
