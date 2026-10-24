package eu.tsilva.homewire.tv;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Intent;
import android.os.IBinder;
import android.os.PowerManager;
import java.util.concurrent.atomic.AtomicReference;

public final class TvService extends Service {
    private ApiServer server;
    private PowerManager.WakeLock standbyLock;
    private static final AtomicReference<PowerManager.WakeLock> activeStandbyLock = new AtomicReference<>();
    static boolean standbyLockHeld() {
        PowerManager.WakeLock lock = activeStandbyLock.get();
        return lock != null && lock.isHeld();
    }
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
            ApiServer connection = new ApiServer(this, Config.load(this));
            server = connection;
            updateStandbyLock();
            new Thread(() -> {
                try { connection.run(); }
                finally { releaseStandbyLock(); stopSelf(); }
            }, "homewire-tv-api").start();
        } catch (Exception e) {
            if (server != null) server.close();
            server = null;
            releaseStandbyLock();
            stopSelf();
        }
    }
    @Override public int onStartCommand(Intent intent, int flags, int id) {
        if (server == null) return START_NOT_STICKY;
        try { updateStandbyLock(); return START_STICKY; }
        catch (RuntimeException e) { stopSelf(); return START_NOT_STICKY; }
    }
    private synchronized void updateStandbyLock() {
        if (!StandbyConnection.enabled(this)) { releaseStandbyLock(); return; }
        if (standbyLock == null) {
            standbyLock = getSystemService(PowerManager.class).newWakeLock(
                PowerManager.PARTIAL_WAKE_LOCK, "homewire-tv:standby-connection");
            standbyLock.setReferenceCounted(false);
        }
        if (!standbyLock.isHeld()) standbyLock.acquire();
        activeStandbyLock.set(standbyLock);
    }
    private synchronized void releaseStandbyLock() {
        PowerManager.WakeLock lock = standbyLock;
        standbyLock = null;
        try { if (lock != null && lock.isHeld()) lock.release(); }
        finally { activeStandbyLock.compareAndSet(lock, null); }
    }
    @Override public void onDestroy() {
        try { if (server != null) server.close(); }
        finally { releaseStandbyLock(); super.onDestroy(); }
    }
    @Override public IBinder onBind(Intent intent) { return null; }
}
