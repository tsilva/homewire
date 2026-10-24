package eu.tsilva.homewire.tv;

import android.content.Context;
import android.os.Build;
import android.util.AtomicFile;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONObject;

final class Config {
    final String token, fingerprint, incremental, device, model;
    final int port;
    Config(JSONObject json) throws Exception {
        if (json.length() != 6) throw new IllegalArgumentException("Invalid pairing fields");
        token = json.getString("token");
        fingerprint = text(json, "fingerprint", 256);
        incremental = text(json, "incremental", 128);
        device = text(json, "device", 128);
        model = text(json, "model", 128);
        Object p = json.get("port");
        if (!(p instanceof Integer)) throw new IllegalArgumentException("Invalid port");
        port = (Integer) p;
        if (!token.matches("[a-f0-9]{64}") || port < 1024 || port > 65535) {
            throw new IllegalArgumentException("Invalid pairing parameters");
        }
        verify();
    }
    void verify() {
        if (Build.VERSION.SDK_INT != 31 || !"TCL".equals(Build.MANUFACTURER)
            || !fingerprint.equals(Build.FINGERPRINT) || !incremental.equals(Build.VERSION.INCREMENTAL)
            || !device.equals(Build.DEVICE) || !model.equals(Build.MODEL)) {
            throw new SecurityException("Device differs from paired firmware");
        }
    }
    private static String text(JSONObject j, String name, int max) throws Exception {
        String s = j.getString(name);
        if (s.isEmpty() || s.length() > max || s.chars().anyMatch(Character::isISOControl)) {
            throw new IllegalArgumentException("Invalid identity");
        }
        return s;
    }
    static AtomicFile file(Context context, String name) {
        return new AtomicFile(new File(context.getFilesDir(), name));
    }
    static byte[] bounded(InputStream in, int max) throws Exception {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        byte[] buffer = new byte[512];
        int n;
        while ((n = in.read(buffer)) != -1) {
            if (out.size() + n > max) throw new IllegalArgumentException("Oversized input");
            out.write(buffer, 0, n);
        }
        return out.toByteArray();
    }
    static Config load(Context context) throws Exception {
        try (InputStream in = file(context, "pairing.json").openRead()) {
            return new Config(new JSONObject(new String(bounded(in, 2048), StandardCharsets.UTF_8)));
        }
    }
    static void save(AtomicFile file, JSONObject value) throws Exception {
        byte[] bytes = value.toString().getBytes(StandardCharsets.UTF_8);
        if (bytes.length > 4096) throw new IllegalArgumentException("Oversized private state");
        FileOutputStream out = file.startWrite();
        try { out.write(bytes); out.getFD().sync(); file.finishWrite(out); }
        catch (Exception e) { file.failWrite(out); throw e; }
    }
}
