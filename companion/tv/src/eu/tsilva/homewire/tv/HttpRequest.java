package eu.tsilva.homewire.tv;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.HashMap;
import java.util.Locale;

/** Small strict parser, independent of Android for host-side negative tests. */
final class HttpRequest {
    final String method, path, authorization;
    final byte[] body;
    private HttpRequest(String method, String path, String authorization, byte[] body) {
        this.method = method; this.path = path; this.authorization = authorization; this.body = body;
    }
    boolean authorized(String token) {
        return authorization != null && MessageDigest.isEqual(
            ("Bearer " + token).getBytes(StandardCharsets.US_ASCII),
            authorization.getBytes(StandardCharsets.US_ASCII));
    }
    static HttpRequest read(InputStream in) throws Exception {
        long deadline = System.nanoTime() + 3_000_000_000L;
        ByteArrayOutputStream header = new ByteArrayOutputStream();
        int end = 0;
        while (end != 0x0d0a0d0a) {
            if (header.size() >= 4096 || System.nanoTime() > deadline) throw new Exception("Headers bound");
            int b = in.read();
            if (b < 0 || b > 127 || b == 0) throw new Exception("Header encoding");
            header.write(b); end = (end << 8) | b;
        }
        String[] lines = header.toString("US-ASCII").split("\r\n");
        String[] start = lines[0].split(" ", -1);
        if (start.length != 3 || !start[2].equals("HTTP/1.1") || start[1].length() > 128
            || !(start[0].equals("GET") || start[0].equals("POST"))) throw new Exception("Request line");
        HashMap<String, String> fields = new HashMap<>();
        for (int i = 1; i < lines.length; i++) {
            int colon = lines[i].indexOf(':');
            if (colon <= 0) throw new Exception("Header field");
            String name = lines[i].substring(0, colon).toLowerCase(Locale.ROOT);
            if (!name.matches("[a-z0-9-]+") || fields.put(name, lines[i].substring(colon + 1).trim()) != null) {
                throw new Exception("Duplicate or invalid header");
            }
        }
        if (fields.containsKey("transfer-encoding") || !fields.containsKey("host")) throw new Exception("Framing");
        String length = fields.getOrDefault("content-length", "0");
        if (!length.matches("0|[1-9][0-9]{0,2}")) throw new Exception("Content length");
        int count = Integer.parseInt(length);
        if (count > 256 || (start[0].equals("GET") && count != 0)
            || (start[0].equals("POST") && !"application/json".equals(fields.get("content-type")))) {
            throw new Exception("Body framing");
        }
        byte[] body = new byte[count];
        for (int n = 0; n < count;) {
            if (System.nanoTime() > deadline) throw new Exception("Body deadline");
            int read = in.read(body, n, count - n);
            if (read <= 0) throw new Exception("Truncated body");
            n += read;
        }
        return new HttpRequest(start[0], start[1], fields.get("authorization"), body);
    }
}
