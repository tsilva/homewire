package eu.tsilva.homewire.tv;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.os.Binder;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.atomic.AtomicBoolean;
import org.json.JSONObject;

/** Initial pairing over an ADB-only pipe. No token is returned or logged. */
public final class PairingProvider extends ContentProvider {
    private final AtomicBoolean writing = new AtomicBoolean();
    public boolean onCreate() { return true; }
    private void operator(Uri uri) {
        if (Binder.getCallingUid() != 2000 || !uri.getPath().equals("/config")) {
            throw new SecurityException("ADB pairing only");
        }
    }
    @Override public ParcelFileDescriptor openFile(Uri uri, String mode) throws java.io.FileNotFoundException {
        operator(uri);
        if (!mode.equals("w") || Config.file(getContext(), "pairing.json").getBaseFile().exists()
            || !writing.compareAndSet(false, true)) throw new SecurityException("Already paired or busy");
        try {
            ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createReliablePipe();
            new Thread(() -> {
                try (InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(pipe[0])) {
                    JSONObject json = new JSONObject(new String(Config.bounded(in, 2048), StandardCharsets.UTF_8));
                    new Config(json);
                    Config.save(Config.file(getContext(), "pairing.json"), json);
                } catch (Exception ignored) { /* Pairing query exposes readiness, never secret values. */ }
                finally { writing.set(false); }
            }, "tv-pair-once").start();
            return pipe[1];
        } catch (Exception e) { writing.set(false); throw new java.io.FileNotFoundException("Pairing pipe unavailable"); }
    }
    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sort) {
        operator(uri);
        long identity = Binder.clearCallingIdentity();
        MatrixCursor result = new MatrixCursor(new String[]{"paired", "port", "certificate_sha256", "configured", "error", "api_state", "tls_error"}, 1);
        boolean configured = false;
        try {
            Config config = Config.load(getContext());
            configured = true;
            result.addRow(new Object[]{1, config.port, Credentials.pin(), 1, "", ApiServer.health, ApiServer.tlsFailure});
        } catch (Exception e) {
            // Operator-only diagnostics disclose exception types, never inputs or messages.
            result.addRow(new Object[]{0, 0, "", configured ? 1 : 0, e.getClass().getSimpleName(), ApiServer.health, ""});
        } finally { Binder.restoreCallingIdentity(identity); }
        return result;
    }
    @Override public String getType(Uri uri) { return "application/octet-stream"; }
    @Override public Uri insert(Uri uri, ContentValues values) { throw new UnsupportedOperationException(); }
    @Override public int update(Uri uri, ContentValues values, String selection, String[] args) { throw new UnsupportedOperationException(); }
    @Override public int delete(Uri uri, String selection, String[] args) { throw new UnsupportedOperationException(); }
}
