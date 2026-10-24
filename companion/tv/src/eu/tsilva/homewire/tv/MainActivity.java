package eu.tsilva.homewire.tv;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.Button;
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
                + "\nBacklight controls use the current picture mode. Standby availability depends on the TV.");
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
            if (getSharedPreferences("lifecycle", 0).getBoolean("enabled", true)) startForegroundService(new Intent(this, TvService.class));
        } catch (Exception e) {
            status.setText("Pair this TV through the Homewire installation helper. No local API is enabled until pairing succeeds.");
        }
    }
}
