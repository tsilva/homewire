package eu.tsilva.homewire.betty;

import android.app.Application;

public final class BettyApplication extends Application {
    Telegram telegram;
    String failure;
    @Override public void onCreate() {
        super.onCreate();
        // A separate APK build has synthetic data for emulator visual QA only.
        if (BuildFlags.PREVIEW) return;
        try { telegram = new Telegram(this); }
        catch (Exception | LinkageError error) { failure = "Unable to open protected Telegram storage. Your existing session has been preserved."; }
    }
}
