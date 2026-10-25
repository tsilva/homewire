package eu.tsilva.homewire.tv;

import android.content.Context;
import java.io.OutputStream;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import javax.net.ssl.SSLServerSocket;
import javax.net.ssl.SSLSocket;
import org.json.JSONObject;

/** One blocking worker, one client, bounded framing; no control retries. */
final class ApiServer implements Runnable {
    static volatile String health = "stopped";
    static volatile String tlsFailure = "";
    private static final Pattern SET = Pattern.compile(
        "\\{\\s*\"backlight_level\"\\s*:\\s*(0|[1-9][0-9]{0,2})\\s*,\\s*\"request_id\"\\s*:\\s*\"([a-f0-9]{32})\"\\s*\\}");
    private static final Pattern NAVIGATE = Pattern.compile(
        "\\{\\s*\"action\"\\s*:\\s*\"(up|down|left|right|select|back|home)\"\\s*,\\s*\"request_id\"\\s*:\\s*\"([a-f0-9]{32})\"\\s*\\}");
    private final Config config;
    private final Display display;
    private final Journal journal;
    private final NavigationJournal navigation;
    private volatile boolean running = true;
    private volatile SSLServerSocket listener;
    private volatile Socket active;
    static final class Failure extends Exception {
        final int code;
        final String reason;
        Failure(int code, String reason) { this.code = code; this.reason = reason; }
    }
    ApiServer(Context context, Config config) throws Exception {
        this.config = config; display = new Display(config); journal = new Journal(context);
        navigation = new NavigationJournal(context);
    }
    public void run() {
        try (SSLServerSocket server = (SSLServerSocket) Credentials.tls().getServerSocketFactory()
                .createServerSocket(config.port, 4)) {
            listener = server;
            server.setEnabledProtocols(new String[]{"TLSv1.2"});
            health = "listening";
            while (running) {
                try (SSLSocket socket = (SSLSocket) server.accept()) {
                    active = socket;
                    socket.setSoTimeout(3000);
                    try { socket.startHandshake(); }
                    catch (javax.net.ssl.SSLException e) {
                        // TLS handshake errors precede all request/credential input.
                        tlsFailure = e.getClass().getSimpleName() + ": " + e.getMessage();
                        if (tlsFailure.length() > 256) tlsFailure = tlsFailure.substring(0, 256);
                        continue;
                    }
                    handle(socket);
                } catch (Exception ignored) { /* No request bytes, credentials or private state in logs. */ }
                finally { active = null; }
            }
        } catch (Exception e) { health = "failed_" + e.getClass().getSimpleName(); }
        finally { listener = null; if (health.equals("listening")) health = "stopped"; }
    }
    void close() {
        running = false;
        try { if (active != null) active.close(); } catch (Exception ignored) { }
        try { if (listener != null) listener.close(); } catch (Exception ignored) { }
    }
    private void handle(SSLSocket socket) throws Exception {
        JSONObject result;
        int code = 200;
        try {
            HttpRequest request;
            try { request = HttpRequest.read(socket.getInputStream()); }
            catch (Exception e) { throw new Failure(400, "invalid_request"); }
            if (!request.authorized(config.token)) throw new Failure(401, "unauthorized");
            config.verify();
            if (request.method.equals("GET") && request.path.equals("/v1/status")) {
                result = display.status();
                journal.reconcile(result.getInt("backlight_level"));
            } else if (request.method.equals("POST") && request.path.equals("/v1/backlight")) {
                Matcher match = SET.matcher(new String(request.body, StandardCharsets.UTF_8));
                if (!match.matches()) throw new Failure(400, "invalid_backlight_request");
                int level = Integer.parseInt(match.group(1));
                if (level > 100) throw new Failure(400, "backlight_out_of_range");
                String id = match.group(2);
                int before = display.get();
                journal.record(id, level, before);
                boolean applied = before != level;
                if (applied) display.set(level);
                JSONObject after = display.status();
                if (after.getInt("backlight_level") != level) throw new Failure(502, "verification_failed_no_replay");
                journal.reconcile(level);
                result = new JSONObject().put("request_id", id).put("applied", applied)
                    .put("verified", true).put("status", after);
            } else if (request.method.equals("GET") && request.path.equals("/v1/prototype/status")) {
                result = VisionService.status();
            } else if (request.method.equals("GET") && request.path.equals("/v1/readback")) {
                result = VisionService.readback();
            } else if (request.method.equals("GET") && request.path.equals("/v1/prototype/screenshot")) {
                respond(socket, 200, "image/jpeg", VisionService.screenshot());
                return;
            } else if (request.method.equals("POST") && request.path.equals("/v1/prototype/navigation")) {
                Matcher match = NAVIGATE.matcher(new String(request.body, StandardCharsets.UTF_8));
                if (!match.matches()) throw new Failure(400, "invalid_navigation_request");
                if (!VisionService.status().getBoolean("enabled")) throw new Failure(409, "accessibility_not_enabled");
                navigation.record(match.group(2));
                result = VisionService.navigate(match.group(1)).put("request_id", match.group(2));
            } else throw new Failure(404, "unknown_endpoint");
        } catch (Failure e) { code = e.code; result = new JSONObject().put("error", e.reason); }
        catch (SecurityException e) { code = 403; result = new JSONObject().put("error", "identity_mismatch"); }
        catch (Exception e) { code = 503; result = new JSONObject().put("error", "display_unavailable_no_replay"); }
        byte[] bytes = result.toString().getBytes(StandardCharsets.UTF_8);
        if (bytes.length > 32768) return;
        respond(socket, code, "application/json", bytes);
    }
    private static void respond(SSLSocket socket, int code, String type, byte[] bytes) throws Exception {
        OutputStream out = socket.getOutputStream();
        String header = "HTTP/1.1 " + code + " Response\r\nContent-Type: " + type + "\r\nContent-Length: "
            + bytes.length + "\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
        out.write(header.getBytes(StandardCharsets.US_ASCII)); out.write(bytes); out.flush();
    }
}
