package eu.tsilva.homewire;

import android.content.SharedPreferences;
import java.io.*;
import java.net.*;
import java.security.*;
import java.security.cert.*;
import javax.net.ssl.*;
import org.json.*;

final class HomewireClient {
  static JSONObject request(SharedPreferences settings, String path, JSONObject body)
      throws Exception {
    String base = settings.getString("url", ""),
        token = settings.getString("token", ""),
        pin = settings.getString("pin", "");
    URL u = new URL(base + path);
    if (!u.getProtocol().equals("https")) throw new IOException();
    HttpsURLConnection c = (HttpsURLConnection) u.openConnection();
    TrustManager[] trust = {
      new X509TrustManager() {
        public X509Certificate[] getAcceptedIssuers() {
          return new X509Certificate[0];
        }

        public void checkClientTrusted(X509Certificate[] x, String a) throws CertificateException {
          throw new CertificateException();
        }

        public void checkServerTrusted(X509Certificate[] x, String a) throws CertificateException {
          try {
            if (x.length == 0
                || !hex(MessageDigest.getInstance("SHA-256").digest(x[0].getEncoded())).equals(pin))
              throw new CertificateException("Certificate mismatch");
          } catch (GeneralSecurityException e) {
            throw new CertificateException(e);
          }
        }
      }
    };
    SSLContext ctx = SSLContext.getInstance("TLS");
    ctx.init(null, trust, new SecureRandom());
    c.setSSLSocketFactory(ctx.getSocketFactory());
    c.setHostnameVerifier((h, s) -> h.equals(u.getHost()));
    c.setConnectTimeout(15000);
    c.setReadTimeout(path.equals("/api/assistant") ? 100000 : 30000);
    c.setInstanceFollowRedirects(false);
    c.setRequestProperty("Authorization", "Bearer " + token);
    try {
      if (body != null) {
        byte[] bytes = body.toString().getBytes("UTF-8");
        c.setRequestMethod("POST");
        c.setRequestProperty("Content-Type", "application/json");
        c.setDoOutput(true);
        c.setFixedLengthStreamingMode(bytes.length);
        try (OutputStream out = c.getOutputStream()) {
          out.write(bytes);
        }
      }
      if (c.getResponseCode() != 200) throw new IOException();
      ByteArrayOutputStream out = new ByteArrayOutputStream();
      try (InputStream in = c.getInputStream()) {
        byte[] b = new byte[1024];
        int n;
        while ((n = in.read(b)) != -1) {
          if (out.size() + n > 65536) throw new IOException();
          out.write(b, 0, n);
        }
      }
      JSONObject rpc = new JSONObject(out.toString("UTF-8"));
      if (rpc.has("error")) throw new IOException();
      return rpc.getJSONObject("result");
    } finally {
      c.disconnect();
    }
  }

  private static String hex(byte[] b) {
    StringBuilder s = new StringBuilder();
    for (byte x : b) s.append(String.format(java.util.Locale.ROOT, "%02x", x & 255));
    return s.toString();
  }
}
