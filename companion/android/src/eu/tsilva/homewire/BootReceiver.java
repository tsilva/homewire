package eu.tsilva.homewire;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public final class BootReceiver extends BroadcastReceiver {
  public void onReceive(Context c, Intent i) {
    if (Intent.ACTION_BOOT_COMPLETED.equals(i.getAction())
        && c.getSharedPreferences("pairing", 0).getBoolean("enabled", false))
      c.startForegroundService(new Intent(c, CompanionService.class));
  }
}
