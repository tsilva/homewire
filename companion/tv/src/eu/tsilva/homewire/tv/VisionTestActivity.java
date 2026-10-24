package eu.tsilva.homewire.tv;

import android.app.Activity;
import android.os.Bundle;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

/** Harmless target for capture, directional focus and selection checks on real hardware. */
public final class VisionTestActivity extends Activity {
    private int selections;
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL); layout.setPadding(64, 48, 64, 48);
        setContentView(layout);
        TextView title = new TextView(this);
        title.setText("Homewire visual navigation test"); title.setTextSize(30); layout.addView(title);
        TextView result = new TextView(this);
        result.setText("Selections: 0"); result.setTextSize(24); layout.addView(result);
        Button first = null;
        for (int row = 0; row < 2; row++) {
            LinearLayout line = new LinearLayout(this); layout.addView(line);
            for (int column = 0; column < 2; column++) {
                String name = new String[]{"Alpha", "Bravo", "Charlie", "Delta"}[row * 2 + column];
                Button button = new Button(this); button.setText(name); button.setTextSize(24);
                line.addView(button, new LinearLayout.LayoutParams(320, 100));
                button.setOnClickListener(v -> result.setText("Selections: " + (++selections) + " / " + name));
                if (first == null) first = button;
            }
        }
        first.requestFocus();
    }
}
