package eu.tsilva.homewire;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.Build;
import android.os.Bundle;
import android.text.InputType;
import android.widget.*;
import java.net.URI;

public final class MainActivity extends Activity {
  private EditText url, pin, token, prompt;
  private TextView status;

  public void onCreate(Bundle b) {
    super.onCreate(b);
    SharedPreferences p = getSharedPreferences("pairing", MODE_PRIVATE);
    LinearLayout l = new LinearLayout(this);
    l.setOrientation(1);
    l.setPadding(32, 32, 32, 32);
    ScrollView scroll = new ScrollView(this);
    scroll.addView(l);
    setContentView(scroll);
    TextView title = new TextView(this);
    title.setText("Homewire companion");
    title.setTextSize(24);
    l.addView(title);
    TextView desc = new TextView(this);
    desc.setText(
        "Reports battery and location and receives home alerts. The persistent notification keeps"
            + " the connection available. Location is optional. Pair with the server URL,"
            + " certificate SHA-256 and a dedicated companion token.");
    l.addView(desc);
    url = field(l, "https://homewire-address:8443", p.getString("url", ""), false);
    pin = field(l, "Certificate SHA-256", p.getString("pin", ""), false);
    token = field(l, "Companion token", p.getString("token", ""), true);
    status = new TextView(this);
    l.addView(status);
    Button start = new Button(this);
    start.setText("Save and start");
    l.addView(start);
    start.setOnClickListener(
        v -> {
          try {
            String u = url.getText().toString().trim(),
                f = pin.getText().toString().trim().toLowerCase(),
                t = token.getText().toString().trim();
            URI uri = new URI(u);
            if (!"https".equals(uri.getScheme())
                || uri.getHost() == null
                || uri.getUserInfo() != null
                || uri.getQuery() != null
                || uri.getFragment() != null
                || (!uri.getPath().isEmpty() && !uri.getPath().equals("/"))
                || !f.matches("[a-f0-9]{64}")
                || !t.matches("[a-fA-F0-9]{64}")) throw new Exception();
            p.edit()
                .putString("url", u.replaceAll("/$", ""))
                .putString("pin", f)
                .putString("token", t)
                .putBoolean("enabled", true)
                .commit();
            token.setText("");
            requestPermissions(
                Build.VERSION.SDK_INT >= 33
                    ? new String[] {
                      Manifest.permission.POST_NOTIFICATIONS,
                      Manifest.permission.ACCESS_FINE_LOCATION,
                      Manifest.permission.ACCESS_COARSE_LOCATION
                    }
                    : new String[] {
                      Manifest.permission.ACCESS_FINE_LOCATION,
                      Manifest.permission.ACCESS_COARSE_LOCATION
                    },
                1);
            status.setText("Settings saved. Allow notifications; location is optional.");
          } catch (Exception e) {
            status.setText("Use an HTTPS URL and 64-digit hexadecimal fingerprint and token.");
          }
        });
    Button stop = new Button(this);
    stop.setText("Stop reporting");
    l.addView(stop);
    stop.setOnClickListener(
        v -> {
          p.edit().putBoolean("enabled", false).commit();
          stopService(new Intent(this, CompanionService.class));
          status.setText("Stopped");
        });
    TextView assistant = new TextView(this);
    assistant.setText(
        "Assistant (optional)\n"
            + "Voice uses your installed Android speech provider, preferring offline recognition."
            + " Review the transcript, then send. The server must grant this companion assistant"
            + " access.");
    l.addView(assistant);
    prompt = field(l, "Ask ZeroClaw", "", false);
    Button voice = new Button(this);
    voice.setText("Dictate");
    l.addView(voice);
    voice.setOnClickListener(
        v -> {
          try {
            Intent r = new Intent(android.speech.RecognizerIntent.ACTION_RECOGNIZE_SPEECH);
            r.putExtra(
                android.speech.RecognizerIntent.EXTRA_LANGUAGE_MODEL,
                android.speech.RecognizerIntent.LANGUAGE_MODEL_FREE_FORM);
            r.putExtra(android.speech.RecognizerIntent.EXTRA_LANGUAGE, "en-US");
            r.putExtra(android.speech.RecognizerIntent.EXTRA_PREFER_OFFLINE, true);
            startActivityForResult(r, 2);
          } catch (android.content.ActivityNotFoundException e) {
            status.setText("No Android speech provider installed. Use text input.");
          }
        });
    Button send = new Button(this);
    send.setText("Send to assistant");
    l.addView(send);
    send.setOnClickListener(
        v -> {
          String message = prompt.getText().toString().trim();
          if (message.isEmpty() || message.length() > 4096) {
            status.setText("Enter a short message first.");
            return;
          }
          send.setEnabled(false);
          status.setText("Waiting for ZeroClaw…");
          new Thread(
                  () -> {
                    String answer;
                    try {
                      answer =
                          HomewireClient.request(
                                  p,
                                  "/api/assistant",
                                  new org.json.JSONObject().put("message", message))
                              .getString("response");
                    } catch (Exception e) {
                      answer =
                          "Assistant request failed. It may already have acted; check device state"
                              + " before sending again.";
                    }
                    final String result = answer;
                    runOnUiThread(
                        () -> {
                          send.setEnabled(true);
                          status.setText(result);
                        });
                  },
                  "Homewire assistant")
              .start();
        });
  }

  protected void onActivityResult(int request, int result, Intent data) {
    super.onActivityResult(request, result, data);
    if (request == 2 && result == RESULT_OK && data != null) {
      java.util.ArrayList<String> text =
          data.getStringArrayListExtra(android.speech.RecognizerIntent.EXTRA_RESULTS);
      if (text != null && !text.isEmpty()) prompt.setText(text.get(0));
    }
  }

  private EditText field(LinearLayout l, String hint, String value, boolean secret) {
    EditText e = new EditText(this);
    e.setHint(hint);
    e.setSingleLine(true);
    e.setText(value);
    e.setInputType(
        InputType.TYPE_CLASS_TEXT
            | (secret
                ? InputType.TYPE_TEXT_VARIATION_PASSWORD
                : InputType.TYPE_TEXT_VARIATION_URI));
    l.addView(e);
    return e;
  }

  public void onRequestPermissionsResult(int code, String[] permissions, int[] grants) {
    super.onRequestPermissionsResult(code, permissions, grants);
    if (code == 1) {
      Intent i = new Intent(this, CompanionService.class);
      i.putExtra("from_activity", true);
      startForegroundService(i);
      status.setText(
          "Companion started. Alerts require network access; delivery may be delayed by Android"
              + " power management.");
    }
  }
}
