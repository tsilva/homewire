package eu.tsilva.homewire.tv;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;

public final class TvService extends Service {
    private ApiServer server;
    @Override public void onCreate() {
        super.onCreate();
        NotificationManager notifications = getSystemService(NotificationManager.class);
        notifications.createNotificationChannel(new NotificationChannel("api", "Homewire TV connection", NotificationManager.IMPORTANCE_LOW));
        startForeground(1, new Notification.Builder(this, "api")
            .setSmallIcon(android.R.drawable.ic_menu_view).setContentTitle("Homewire TV")
            .setContentText("Authenticated local picture controls are running")
            .setContentIntent(PendingIntent.getActivity(this, 0, new Intent(this, MainActivity.class), PendingIntent.FLAG_IMMUTABLE))
            .setOngoing(true).build());
        try {
            server = new ApiServer(this, Config.load(this));
            new Thread(server, "homewire-tv-api").start();
        } catch (Exception e) { stopSelf(); }
    }
    @Override public int onStartCommand(Intent intent, int flags, int id) { return START_STICKY; }
    @Override public void onDestroy() { if (server != null) server.close(); super.onDestroy(); }
    @Override public IBinder onBind(Intent intent) { return null; }
}
