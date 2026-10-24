package eu.tsilva.homewire.tv;

import android.accessibilityservice.AccessibilityService;
import android.graphics.Bitmap;
import android.graphics.Rect;
import android.hardware.HardwareBuffer;
import android.os.Handler;
import android.os.Looper;
import android.view.View;
import android.view.accessibility.AccessibilityEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import java.io.ByteArrayOutputStream;
import java.util.concurrent.Callable;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.FutureTask;
import java.util.concurrent.TimeUnit;
import org.json.JSONObject;

/** Opt-in experiment. No retained images, node trees, event history or input injection. */
public final class VisionService extends AccessibilityService {
    private static volatile VisionService connected;
    private final Handler main = new Handler(Looper.getMainLooper());
    @Override protected void onServiceConnected() { connected = this; }
    @Override public void onAccessibilityEvent(AccessibilityEvent event) { }
    @Override public void onInterrupt() { }
    @Override public void onDestroy() {
        if (connected == this) connected = null;
        super.onDestroy();
    }
    private static VisionService require() throws ApiServer.Failure {
        VisionService service = connected;
        if (service == null) throw new ApiServer.Failure(409, "accessibility_not_enabled");
        return service;
    }
    private <T> T onMain(Callable<T> operation) throws Exception {
        FutureTask<T> task = new FutureTask<>(operation);
        if (!main.post(task)) throw new ApiServer.Failure(503, "accessibility_unavailable_no_replay");
        try { return task.get(2, TimeUnit.SECONDS); }
        finally { task.cancel(false); main.removeCallbacks(task); }
    }
    static JSONObject status() throws Exception {
        VisionService service = connected;
        if (service == null) return new JSONObject().put("enabled", false);
        return service.onMain(() -> new JSONObject().put("enabled", true)
            .put("navigation_method", "accessibility_focus_actions")
            .put("focus", service.focus()));
    }
    private JSONObject focus() throws Exception {
        AccessibilityNodeInfo node = findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
        if (node == null) return new JSONObject().put("available", false);
        try {
            Rect bounds = new Rect(); node.getBoundsInScreen(bounds);
            return new JSONObject().put("available", true).put("package", text(node.getPackageName()))
                .put("class", text(node.getClassName())).put("text", text(node.getText()))
                .put("description", text(node.getContentDescription()))
                .put("bounds", bounds.toShortString());
        } finally { node.recycle(); }
    }
    private static String text(CharSequence value) {
        if (value == null) return "";
        return value.subSequence(0, Math.min(160, value.length())).toString();
    }
    static JSONObject navigate(String action) throws Exception {
        VisionService service = require();
        return service.onMain(() -> {
            boolean accepted;
            if (action.equals("home") || action.equals("back")) {
                accepted = service.performGlobalAction(action.equals("home") ? GLOBAL_ACTION_HOME : GLOBAL_ACTION_BACK);
            } else {
                AccessibilityNodeInfo current = service.findFocus(AccessibilityNodeInfo.FOCUS_INPUT);
                if (current == null) return new JSONObject().put("accepted", false).put("reason", "no_input_focus");
                try {
                    if (action.equals("select")) accepted = current.performAction(AccessibilityNodeInfo.ACTION_CLICK);
                    else {
                        int direction;
                        switch (action) {
                            case "up": direction = View.FOCUS_UP; break;
                            case "down": direction = View.FOCUS_DOWN; break;
                            case "left": direction = View.FOCUS_LEFT; break;
                            case "right": direction = View.FOCUS_RIGHT; break;
                            default: throw new IllegalArgumentException("Invalid navigation action");
                        }
                        AccessibilityNodeInfo next = current.focusSearch(direction);
                        if (next == null) accepted = false;
                        else {
                            try { accepted = next.performAction(AccessibilityNodeInfo.ACTION_FOCUS); }
                            finally { next.recycle(); }
                        }
                    }
                } finally { current.recycle(); }
            }
            // Accepted is Android's acknowledgement, never proof of the resulting screen.
            return new JSONObject().put("accepted", accepted).put("verified", false);
        });
    }
    static byte[] screenshot() throws Exception {
        VisionService service = require();
        CompletableFuture<ScreenshotResult> result = new CompletableFuture<>();
        service.takeScreenshot(android.view.Display.DEFAULT_DISPLAY, service.getMainExecutor(), new TakeScreenshotCallback() {
            @Override public void onSuccess(ScreenshotResult shot) {
                if (!result.complete(shot)) shot.getHardwareBuffer().close();
            }
            @Override public void onFailure(int errorCode) {
                result.completeExceptionally(new IllegalStateException("Screenshot unavailable"));
            }
        });
        ScreenshotResult shot;
        try { shot = result.get(2, TimeUnit.SECONDS); }
        catch (Exception e) {
            // A callback racing the timeout must also release its native buffer.
            if (!result.cancel(false) && !result.isCompletedExceptionally()) {
                ScreenshotResult late = result.getNow(null);
                if (late != null) late.getHardwareBuffer().close();
            }
            throw new ApiServer.Failure(503, "screenshot_unavailable");
        }
        Bitmap hardware = null, software = null, scaled = null;
        try (HardwareBuffer buffer = shot.getHardwareBuffer()) {
            if (buffer.getWidth() < 1 || buffer.getHeight() < 1
                || (long) buffer.getWidth() * buffer.getHeight() > 8294400) {
                throw new ApiServer.Failure(503, "screenshot_dimensions_unsupported");
            }
            hardware = Bitmap.wrapHardwareBuffer(buffer, shot.getColorSpace());
            if (hardware == null) throw new ApiServer.Failure(503, "screenshot_unavailable");
            software = hardware.copy(Bitmap.Config.ARGB_8888, false);
            if (software == null) throw new ApiServer.Failure(503, "screenshot_unavailable");
            double ratio = Math.min(1.0, Math.min(1280.0 / software.getWidth(), 720.0 / software.getHeight()));
            scaled = Bitmap.createScaledBitmap(software, Math.max(1, (int) (software.getWidth() * ratio)),
                Math.max(1, (int) (software.getHeight() * ratio)), true);
            ByteArrayOutputStream bytes = new ByteArrayOutputStream() {
                private void limit(int length) {
                    if (length < 0 || count + (long) length > 2097152) throw new IllegalStateException("Screenshot too large");
                }
                @Override public synchronized void write(int value) { limit(1); super.write(value); }
                @Override public synchronized void write(byte[] value, int offset, int length) {
                    limit(length); super.write(value, offset, length);
                }
            };
            if (!scaled.compress(Bitmap.CompressFormat.JPEG, 80, bytes)) {
                throw new ApiServer.Failure(503, "screenshot_encoding_failed");
            }
            return bytes.toByteArray();
        } finally {
            if (scaled != null && scaled != software) scaled.recycle();
            if (software != null) software.recycle();
            if (hardware != null) hardware.recycle();
        }
    }
}
