package eu.tsilva.homewire.tv;
import java.io.ByteArrayInputStream;
import java.nio.charset.StandardCharsets;

public final class HttpRequestTest {
    private static HttpRequest parse(String value) throws Exception {
        return HttpRequest.read(new ByteArrayInputStream(value.getBytes(StandardCharsets.US_ASCII)));
    }
    private static void rejected(String value) throws Exception {
        try { parse(value); } catch (Exception expected) { return; }
        throw new AssertionError("Malformed request accepted");
    }
    public static void main(String[] args) throws Exception {
        String token = new String(new char[64]).replace('\0', 'a');
        HttpRequest read = parse("GET /v1/status HTTP/1.1\r\nHost: tv\r\nAuthorization: Bearer " + token + "\r\n\r\n");
        if (!read.authorized(token) || read.authorized(token + "a") || read.authorized("b" + token.substring(1))) throw new AssertionError("Authentication comparison");
        if (parse("GET /v1/status HTTP/1.1\r\nHost: tv\r\n\r\n").authorized(token)) throw new AssertionError("Missing auth");
        rejected("GET /v1/status HTTP/1.1\r\nHost: tv\r\nAuthorization: a\r\nAuthorization: b\r\n\r\n");
        rejected("POST /v1/backlight HTTP/1.1\r\nHost: tv\r\nTransfer-Encoding: chunked\r\n\r\n");
        rejected("POST /v1/backlight HTTP/1.1\r\nHost: tv\r\nContent-Length: 257\r\n\r\n");
        rejected("POST /v1/backlight HTTP/1.1\r\nHost: tv\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\nx");
        rejected("GET /v1/status HTTP/1.1\r\nHost: tv\r\nContent-Length: 1\r\n\r\nx");
        rejected("GET /v1/status HTTP/1.1\r\nX: " + new String(new char[4096]).replace('\0','x') + "\r\n\r\n");
        System.out.println("TV HTTP framing and authentication tests passed");
    }
}
