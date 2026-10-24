package eu.tsilva.homewire.tv;

import android.os.Build;
import android.os.IBinder;
import android.os.Parcel;
import org.json.JSONObject;

final class Display {
    private static final String DESCRIPTOR = "com.tcl.tv.display.ITvDisplayManagerService";
    private final Config config;
    Display(Config config) { this.config = config; }
    private IBinder service() throws Exception {
        config.verify();
        IBinder binder = (IBinder) Class.forName("android.os.ServiceManager")
            .getDeclaredMethod("getService", String.class).invoke(null, "tcl_tv_display");
        if (binder == null || !DESCRIPTOR.equals(binder.getInterfaceDescriptor())) {
            throw new SecurityException("Unexpected display service");
        }
        return binder;
    }
    int get() throws Exception {
        IBinder binder = service();
        Parcel data = Parcel.obtain(), reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(DESCRIPTOR);
            if (!binder.transact(101, data, reply, 0)) throw new IllegalStateException("Getter rejected");
            reply.readException();
            if (reply.dataAvail() != 4) throw new IllegalStateException("Invalid getter response");
            int value = reply.readInt();
            if (value < 0 || value > 100) throw new IllegalStateException("Invalid backlight range");
            return value;
        } finally { reply.recycle(); data.recycle(); }
    }
    void set(int level) throws Exception {
        if (level < 0 || level > 100) throw new IllegalArgumentException("Backlight range");
        IBinder binder = service();
        Parcel data = Parcel.obtain(), reply = Parcel.obtain();
        try {
            data.writeInterfaceToken(DESCRIPTOR);
            data.writeInt(0); // Current picture mode.
            data.writeInt(level);
            data.writeInt(2); // Apply and save, matching the TV menu.
            if (!binder.transact(100, data, reply, 0)) throw new IllegalStateException("Setter rejected");
            reply.readException();
            if (reply.dataAvail() != 4 || reply.readInt() == 0) {
                throw new IllegalStateException("Setter unconfirmed");
            }
        } finally { reply.recycle(); data.recycle(); }
    }
    JSONObject status() throws Exception {
        return new JSONObject().put("protocol_version", 1).put("manufacturer", Build.MANUFACTURER)
            .put("model", Build.MODEL).put("device", Build.DEVICE).put("fingerprint", Build.FINGERPRINT)
            .put("incremental", Build.VERSION.INCREMENTAL).put("backlight_level", get());
    }
}
