package eu.tsilva.homewire.tv;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.LinearLayout;
import android.widget.TextView;

public final class MainActivity extends Activity {
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this); layout.setOrientation(LinearLayout.VERTICAL); layout.setPadding(48, 32, 48, 32);
        setContentView(layout);
        TextView title = new TextView(this); title.setText("Homewire TV"); title.setTextSize(26); layout.addView(title);
        TextView status = new TextView(this); layout.addView(status);
        try {
            Config config = Config.load(this);
            status.setText("Paired TCL TV • HTTPS port " + config.port + "\nCertificate: " + Credentials.pin()
                + "\nBacklight controls use the current picture mode.");
            CheckBox standby = new CheckBox(this);
            standby.setText("Keep connection available in standby (uses more power)");
            standby.setChecked(StandbyConnection.enabled(this)); layout.addView(standby);
            standby.setOnClickListener(v -> {
                try { StandbyConnection.set(this, standby.isChecked()); }
                catch (Exception e) {
                    standby.setChecked(StandbyConnection.enabled(this));
                    status.setText("Standby connection could not be applied. Check the connection service.");
                }
            });
            Button start = new Button(this); start.setText("Start connection"); layout.addView(start);
            start.setOnClickListener(v -> {
                getSharedPreferences("lifecycle", 0).edit().putBoolean("enabled", true).commit();
                startForegroundService(new Intent(this, TvService.class));
            });
            Button stop = new Button(this); stop.setText("Stop connection"); layout.addView(stop);
            stop.setOnClickListener(v -> {
                getSharedPreferences("lifecycle", 0).edit().putBoolean("enabled", false).commit();
                stopService(new Intent(this, TvService.class));
            });
            Button vision = new Button(this); vision.setText("Visual prototype: accessibility settings"); layout.addView(vision);
            vision.setOnClickListener(v -> startActivity(new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS)));
            TextView readback = new TextView(this);
            readback.setText("Optional playback access reads Android media sessions, not notification contents. "
                + "Apps may omit their title metadata.");
            layout.addView(readback);
            Button playback = new Button(this); playback.setText("Playback state: notification access settings"); layout.addView(playback);
            playback.setOnClickListener(v -> {
                try { startActivity(new Intent(android.provider.Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)); }
                catch (android.content.ActivityNotFoundException e) { status.setText("Notification access settings are unavailable on this TV."); }
            });
            if (getSharedPreferences("lifecycle", 0).getBoolean("enabled", true)) startForegroundService(new Intent(this, TvService.class));
        } catch (Exception e) {
            status.setText("Pair this TV through the Homewire installation helper. No local API is enabled until pairing succeeds.");
        }
    }
}
