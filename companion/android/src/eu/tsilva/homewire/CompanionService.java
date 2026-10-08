package eu.tsilva.homewire;

import android.Manifest;
import android.app.*;
import android.content.*;
import android.content.pm.*;
import android.location.*;
import android.os.*;
import java.io.*;
import java.net.*;
import java.security.*;
import java.security.cert.*;
import javax.net.ssl.*;
import org.json.*;

public final class CompanionService extends Service implements LocationListener {
  private final Object wake = new Object();
  private volatile boolean running;
  private Thread worker;
  private Location location;
  private LocationManager locations;
  private SharedPreferences settings, receipts;
  private NotificationManager notifications;
  private BroadcastReceiver battery;

  public void onCreate() {
    super.onCreate();
    settings = getSharedPreferences("pairing", 0);
    receipts = getSharedPreferences("receipts", 0);
    notifications = getSystemService(NotificationManager.class);
    notifications.createNotificationChannel(
        new NotificationChannel(
            "connection", "Homewire connection", NotificationManager.IMPORTANCE_LOW));
    notifications.createNotificationChannel(
        new NotificationChannel("alerts", "Home alerts", NotificationManager.IMPORTANCE_HIGH));
    locations = getSystemService(LocationManager.class);
    battery =
        new BroadcastReceiver() {
          public void onReceive(Context c, Intent i) {
            synchronized (wake) {
              wake.notifyAll();
            }
          }
        };
    registerReceiver(battery, new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
  }

  public int onStartCommand(Intent i, int flags, int id) {
    if (!settings.getBoolean("enabled", false)) {
      stopSelf();
      return START_NOT_STICKY;
    }
    boolean permitted =
        checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION)
                == PackageManager.PERMISSION_GRANTED
            || checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION)
                == PackageManager.PERMISSION_GRANTED;
    boolean foreground = i != null && i.getBooleanExtra("from_activity", false);
    int types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE;
    if (permitted && foreground) types |= ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION;
    startForeground(
        1,
        new Notification.Builder(this, "connection")
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("Homewire companion")
            .setContentText("Home alerts and battery reporting active")
            .setOngoing(true)
            .setContentIntent(
                PendingIntent.getActivity(
                    this, 0, new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE))
            .build(),
        types);
    if (permitted && foreground) {
      try {
        locations.requestLocationUpdates(LocationManager.PASSIVE_PROVIDER, 300000, 50, this);
        if (locations.isProviderEnabled(LocationManager.NETWORK_PROVIDER))
          locations.requestLocationUpdates(LocationManager.NETWORK_PROVIDER, 300000, 50, this);
      } catch (SecurityException ignored) {
      }
    }
    if (worker == null) {
      running = true;
      worker = new Thread(() -> loop(), "Homewire companion");
      worker.start();
    }
    return START_STICKY;
  }

  private void loop() {
    while (running) {
      long start = SystemClock.elapsedRealtime();
      try {
        JSONObject report = new JSONObject();
        Intent b = registerReceiver(null, new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
        if (b == null) throw new IOException();
        int scale = b.getIntExtra(BatteryManager.EXTRA_SCALE, 100);
        report.put(
            "battery_percent",
            Math.min(
                100,
                Math.max(
                    0, b.getIntExtra(BatteryManager.EXTRA_LEVEL, 0) * 100 / Math.max(1, scale))));
        int state = b.getIntExtra(BatteryManager.EXTRA_STATUS, 1);
        report.put(
            "battery_state",
            state == BatteryManager.BATTERY_STATUS_CHARGING
                ? "charging"
                : state == BatteryManager.BATTERY_STATUS_FULL
                    ? "full"
                    : state == BatteryManager.BATTERY_STATUS_DISCHARGING
                            || state == BatteryManager.BATTERY_STATUS_NOT_CHARGING
                        ? "discharging"
                        : "unknown");
        int plugged = b.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0);
        report.put(
            "charger",
            plugged == BatteryManager.BATTERY_PLUGGED_AC
                ? "ac"
                : plugged == BatteryManager.BATTERY_PLUGGED_USB
                    ? "usb"
                    : plugged == BatteryManager.BATTERY_PLUGGED_WIRELESS
                        ? "wireless"
                        : plugged == 0 ? "none" : "unknown");
        Location loc;
        synchronized (wake) {
          loc = location == null ? null : new Location(location);
        }
        if (loc != null && loc.hasAccuracy()) {
          JSONObject l = new JSONObject();
          l.put("latitude", loc.getLatitude());
          l.put("longitude", loc.getLongitude());
          l.put("accuracy_m", loc.getAccuracy());
          l.put("measured_at", loc.getTime() / 1000);
          report.put("location", l);
        }
        request("/api/mobile/report", report);
        JSONArray alerts = request("/api/mobile/alerts", null).getJSONArray("alerts");
        for (int n = 0; n < alerts.length() && running; n++) {
          JSONObject a = alerts.getJSONObject(n);
          String key = a.getString("key"), revision = a.getString("revision");
          if (!revision.equals(receipts.getString(key, ""))) {
            if (a.getBoolean("active"))
              notifications.notify(
                  key,
                  2,
                  new Notification.Builder(this, "alerts")
                      .setSmallIcon(android.R.drawable.stat_notify_error)
                      .setContentTitle(a.getString("title"))
                      .setContentText(a.getString("message"))
                      .setStyle(new Notification.BigTextStyle().bigText(a.getString("message")))
                      .setOnlyAlertOnce(true)
                      .setContentIntent(
                          PendingIntent.getActivity(
                              this,
                              0,
                              new Intent(this, MainActivity.class),
                              PendingIntent.FLAG_IMMUTABLE))
                      .build());
            else notifications.cancel(key, 2);
            if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                    != PackageManager.PERMISSION_GRANTED) continue;
            if (!receipts.contains(key) && receipts.getAll().size() >= 64) {
              String old = receipts.getAll().keySet().iterator().next();
              receipts.edit().remove(old).commit();
            }
            receipts.edit().putString(key, revision).commit();
          }
          request("/api/mobile/ack", new JSONObject().put("key", key).put("revision", revision));
        }
      } catch (Exception ignored) {
        /* Credentials and response bodies never enter logs. Next poll retries reads/acks only. */
      }
      synchronized (wake) {
        try {
          wake.wait(Math.max(10000, 60000 - (SystemClock.elapsedRealtime() - start)));
        } catch (InterruptedException e) {
          return;
        }
      }
    }
  }

  private JSONObject request(String path, JSONObject body) throws Exception {
    return HomewireClient.request(settings, path, body);
  }

  public void onLocationChanged(Location l) {
    synchronized (wake) {
      location = new Location(l);
      wake.notifyAll();
    }
  }

  public void onDestroy() {
    running = false;
    if (worker != null) worker.interrupt();
    synchronized (wake) {
      wake.notifyAll();
    }
    if (locations != null) locations.removeUpdates(this);
    if (battery != null) unregisterReceiver(battery);
    super.onDestroy();
  }

  public IBinder onBind(Intent i) {
    return null;
  }
}
