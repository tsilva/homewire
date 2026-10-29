package eu.tsilva.homewire.betty;

import android.content.Context;
import android.os.Handler;
import android.os.Looper;
import android.util.Base64;
import org.drinkless.tdlib.Client;
import org.drinkless.tdlib.TdApi;
import org.json.JSONObject;
import java.io.File;
import java.util.Arrays;
import java.util.concurrent.Semaphore;
import java.util.function.Consumer;

/** A user client, never a bot-token client or a second consumer of Betty's updates. */
final class Telegram {
    interface Listener { void changed(); void delivered(); }
    final Handler main = new Handler(Looper.getMainLooper());
    final Vault vault;
    final JSONObject config;
    final Context context;
    final Semaphore slots = new Semaphore(24);
    Client client;
    Listener listener;
    TdApi.AuthorizationState auth;
    TdApi.Chat candidate;
    String status = "Connect your Telegram account";
    volatile long chatId;
    long pendingId;
    boolean ready, active, sendBusy, sendUnknown;
    String connection = "Connecting";

    Telegram(Context context) throws Exception {
        this.context = context;
        vault = new Vault(context); config = vault.read();
        chatId = config.optLong("chat");
        sendUnknown = config.optBoolean("pending"); pendingId = config.optLong("pending_id");
        // These files contain TDLib/OpenSSL, pinned together in dependencies.lock.json.
        System.loadLibrary("cryptox"); System.loadLibrary("sslx"); System.loadLibrary("tdjni");
        Client.execute(new TdApi.SetLogStream(new TdApi.LogStreamEmpty()));
        Client.execute(new TdApi.SetLogVerbosityLevel(0));
        if (configured()) start();
    }

    boolean configured() { return config.optInt("api_id") > 0 && config.optString("api_hash").matches("[a-fA-F0-9]{32}"); }
    void notifyUi() { if (listener != null) listener.changed(); }
    void error(String message) { status = message; notifyUi(); }
    boolean persist() {
        try { vault.write(config); return true; }
        catch (Exception e) { error("Protected storage could not be saved. Check Telegram before sending again."); return false; }
    }

    void configure(int apiId, String hash, String username) throws Exception {
        if (client != null) throw new IllegalStateException("Session already started");
        if (apiId <= 0 || !hash.matches("[a-fA-F0-9]{32}") || !username.matches("[a-zA-Z][a-zA-Z0-9_]{4,31}"))
            throw new IllegalArgumentException("Check the API ID, API hash and bot username");
        config.put("api_id", apiId).put("api_hash", hash).put("username", username);
        if (persist()) start();
    }

    void start() {
        client = Client.create(this::update, error -> main.post(() -> error("Telegram stopped responding. Reopen the app.")),
            error -> main.post(() -> error("Telegram could not complete this request.")));
    }

    void request(TdApi.Function<?> function, Consumer<TdApi.Object> result) {
        if (client == null || !slots.tryAcquire()) { error("Telegram is busy. Wait for the current request."); return; }
        client.send(function, object -> {
            slots.release();
            main.post(() -> result.accept(object));
        }, throwable -> { slots.release(); main.post(() -> error("Telegram could not complete this request.")); });
    }

    private void update(TdApi.Object object) {
        if (object instanceof TdApi.UpdateAuthorizationState) {
            TdApi.AuthorizationState next = ((TdApi.UpdateAuthorizationState) object).authorizationState;
            main.post(() -> { auth = next; authorization(); });
        } else if (object instanceof TdApi.UpdateConnectionState) {
            TdApi.ConnectionState state = ((TdApi.UpdateConnectionState) object).state;
            String value = state instanceof TdApi.ConnectionStateReady ? "Connected" :
                state instanceof TdApi.ConnectionStateWaitingForNetwork ? "Waiting for network" : "Connecting";
            main.post(() -> { connection = value; notifyUi(); });
        } else if (object instanceof TdApi.UpdateMessageSendSucceeded) {
            TdApi.UpdateMessageSendSucceeded sent = (TdApi.UpdateMessageSendSucceeded) object;
            if (sent.message.chatId == chatId) main.post(() -> {
                if ((sendBusy || sendUnknown) && pendingId == sent.oldMessageId) sent();
            });
        } else if (object instanceof TdApi.UpdateMessageSendFailed) {
            TdApi.UpdateMessageSendFailed failed = (TdApi.UpdateMessageSendFailed) object;
            if (failed.message.chatId == chatId) main.post(() -> {
                if ((sendBusy || sendUnknown) && pendingId == failed.oldMessageId) uncertain("Telegram reports a send failure. Check the chat before sending again.");
            });
        }
        // Unrelated chat/user/file updates are released immediately, never retained by the UI.
    }

    void authorization() {
        ready = false;
        if (auth instanceof TdApi.AuthorizationStateWaitTdlibParameters) {
            try {
                byte[] databaseKey;
                if (config.has("database_key")) databaseKey = Base64.decode(config.getString("database_key"), Base64.NO_WRAP);
                else {
                    databaseKey = Vault.randomKey();
                    config.put("database_key", Base64.encodeToString(databaseKey, Base64.NO_WRAP));
                    if (!persist()) return;
                }
                File database = new File(context.getNoBackupFilesDir(), "telegram");
                File files = new File(context.getCacheDir(), "telegram");
                if ((!database.isDirectory() && !database.mkdirs()) || (!files.isDirectory() && !files.mkdirs()))
                    throw new IllegalStateException();
                TdApi.SetTdlibParameters parameters = new TdApi.SetTdlibParameters();
                parameters.databaseDirectory = database.getAbsolutePath(); parameters.filesDirectory = files.getAbsolutePath();
                parameters.databaseEncryptionKey = databaseKey;
                // Auth persists, but history for the rest of the account is not cached on disk.
                parameters.useFileDatabase = false; parameters.useChatInfoDatabase = false;
                parameters.useMessageDatabase = false; parameters.useSecretChats = false;
                parameters.apiId = config.getInt("api_id"); parameters.apiHash = config.getString("api_hash");
                parameters.systemLanguageCode = java.util.Locale.getDefault().toLanguageTag();
                parameters.deviceModel = "Betty Android"; parameters.systemVersion = android.os.Build.VERSION.RELEASE;
                parameters.applicationVersion = "0.1.4";
                request(parameters, result -> {
                    Arrays.fill(databaseKey, (byte) 0);
                    if (result instanceof TdApi.Error) error("Telegram initialization failed. Check your API credentials.");
                });
            } catch (Exception e) { error("Protected Telegram storage could not be opened. Your session was preserved."); }
        } else if (auth instanceof TdApi.AuthorizationStateReady) {
            status = "Opening your existing conversation…";
            request(new TdApi.GetMe(), result -> {
                if (!(result instanceof TdApi.User)) { error("Could not verify your Telegram account."); return; }
                long account = ((TdApi.User) result).id;
                if (config.optLong("account") != 0 && config.optLong("account") != account) {
                    error("This is a different Telegram account. Sign in with the account used for your saved conversation."); return;
                }
                try { config.put("account", account); } catch (Exception impossible) { return; }
                if (persist()) resolve();
            });
        } else if (auth instanceof TdApi.AuthorizationStateWaitPhoneNumber) status = "Sign in to your existing Telegram account";
        else if (auth instanceof TdApi.AuthorizationStateWaitCode) status = "Enter the login code sent by Telegram";
        else if (auth instanceof TdApi.AuthorizationStateWaitPassword) status = "Enter your Telegram two-step verification password";
        else if (auth instanceof TdApi.AuthorizationStateClosed) { ready = false; status = "Telegram session closed. Reopen the app."; }
        else if (auth instanceof TdApi.AuthorizationStateWaitEmailAddress || auth instanceof TdApi.AuthorizationStateWaitEmailCode)
            status = "Telegram requires email verification. Complete the requested step below.";
        else if (auth instanceof TdApi.AuthorizationStateWaitRegistration)
            status = "Use an existing Telegram account. New account registration is not supported.";
        else if (auth instanceof TdApi.AuthorizationStateWaitOtherDeviceConfirmation)
            status = "Confirm this new session in your existing Telegram app.";
        else status = "Telegram is preparing your session…";
        notifyUi();
    }

    void authenticate(String value) {
        TdApi.Function<?> function;
        if (auth instanceof TdApi.AuthorizationStateWaitPhoneNumber)
            function = new TdApi.SetAuthenticationPhoneNumber(value, new TdApi.PhoneNumberAuthenticationSettings());
        else if (auth instanceof TdApi.AuthorizationStateWaitCode) function = new TdApi.CheckAuthenticationCode(value);
        else if (auth instanceof TdApi.AuthorizationStateWaitPassword) function = new TdApi.CheckAuthenticationPassword(value);
        else if (auth instanceof TdApi.AuthorizationStateWaitEmailAddress) function = new TdApi.SetAuthenticationEmailAddress(value);
        else if (auth instanceof TdApi.AuthorizationStateWaitEmailCode)
            function = new TdApi.CheckAuthenticationEmailCode(new TdApi.EmailAddressAuthenticationCode(value));
        else { error("This Telegram login step must be completed in Telegram."); return; }
        status = "Checking with Telegram…"; notifyUi();
        request(function, result -> {
            if (result instanceof TdApi.Error) {
                TdApi.Error failure = (TdApi.Error) result;
                error(Login.failure(failure.code, failure.message));
            }
        });
    }

    void resolve() {
        Destination.resolve(this::request, config.optString("username"), chatId, config.optLong("user"), chat -> {
            if (chatId == 0) { candidate = chat; status = "Confirm the existing bot conversation"; notifyUi(); }
            else { candidate = null; ready = true; status = sendUnknown ? "Check your last send in Telegram before continuing." : ""; setActive(active); notifyUi(); }
        }, this::error);
    }

    void confirmChat() {
        if (candidate == null || !(candidate.type instanceof TdApi.ChatTypePrivate)) return;
        TdApi.Chat selected = candidate;
        // Refuse to start a fresh bot conversation: the user asked to keep an existing one.
        request(new TdApi.GetChatHistory(selected.id, 0, 0, 1, false), result -> {
            if (!(result instanceof TdApi.Messages) || ((TdApi.Messages) result).messages.length == 0) {
                error("No existing messages were found. Verify the bot and your account in Telegram first."); return;
            }
            try {
                config.put("chat", selected.id).put("user", ((TdApi.ChatTypePrivate) selected.type).userId);
                if (!persist()) return;
                chatId = selected.id; candidate = null; ready = true; status = "";
                setActive(active); notifyUi();
            } catch (Exception e) { error("The selected conversation could not be saved."); }
        });
    }

    void setActive(boolean value) {
        active = value;
        if (client == null || !ready) return;
        request(new TdApi.SetOption("online", new TdApi.OptionValueBoolean(value)), result -> {});
    }

    boolean sendText(String text) {
        if (text.trim().isEmpty() || text.length() > 4096) { error("Use a message between 1 and 4096 characters."); return false; }
        return send(new TdApi.InputMessageText(new TdApi.FormattedText(text, new TdApi.TextEntity[0]), null, false));
    }
    private boolean send(TdApi.InputMessageContent content) {
        if (!ready || !active || sendBusy || sendUnknown || !connection.equals("Connected")) {
            error("Wait for Telegram to connect and any previous send to finish."); return false;
        }
        try { config.put("pending", true); } catch (Exception impossible) { return false; }
        if (!persist()) return false;
        sendBusy = true; pendingId = 0; status = "Sending to your Telegram conversation…";
        request(new TdApi.SendMessage(chatId, null, null, null, null, content), result -> {
            if (result instanceof TdApi.Message) {
                TdApi.Message message = (TdApi.Message) result; pendingId = message.id;
                try { config.put("pending_id", pendingId); } catch (Exception ignored) { }
                if (!persist()) { uncertain("The send may have reached Telegram. Check your chat before continuing."); return; }
                if (message.sendingState == null) sent();
                else if (message.sendingState instanceof TdApi.MessageSendingStateFailed) uncertain("The message failed. Check Telegram before sending again.");
            } else uncertain("Send result is uncertain. Check Telegram before sending again.");
            notifyUi();
        });
        final long startedAt = android.os.SystemClock.elapsedRealtime();
        sendStarted = startedAt;
        main.postDelayed(() -> { if (sendBusy && sendStarted == startedAt) uncertain("Telegram has not confirmed delivery yet. Check the chat; no second message was sent."); }, 45000);
        notifyUi(); return true;
    }
    long sendStarted;
    void sent() {
        if (!sendBusy && !sendUnknown) return;
        sendBusy = false; sendUnknown = true;
        try { config.put("pending", false).put("pending_id", 0); } catch (Exception ignored) { }
        if (!persist()) {
            try { config.put("pending", true).put("pending_id", pendingId); } catch (Exception ignored) { }
            uncertain("Telegram confirmed the message, but local storage could not be saved. Check the chat before continuing.");
            return;
        }
        sendUnknown = false; pendingId = 0;
        status = "Sent to Telegram";
        if (listener != null) listener.delivered();
        notifyUi();
    }
    void uncertain(String message) { sendBusy = false; sendUnknown = true; status = message; notifyUi(); }
    void acknowledgeUncertain() {
        if (sendBusy) return;
        if (pendingId != 0) {
            request(new TdApi.GetMessage(chatId, pendingId), result -> {
                if (result instanceof TdApi.Message && ((TdApi.Message) result).sendingState instanceof TdApi.MessageSendingStatePending) {
                    error("Telegram is still processing this message. Wait for its final result; no new send is enabled."); return;
                }
                clearUncertain();
            });
            return;
        }
        clearUncertain();
    }
    private void clearUncertain() {
        // This acknowledges the result only. It never repeats SendMessage or deletes a Telegram message.
        try { config.put("pending", false).put("pending_id", 0); } catch (Exception ignored) { }
        if (!persist()) return;
        sendUnknown = false; pendingId = 0;
        status = "Previous send acknowledged. Review the chat before composing again."; notifyUi();
    }
    void logout() {
        if (sendBusy || sendUnknown) { error("Check the pending send before signing out."); return; }
        request(new TdApi.LogOut(), result -> { if (result instanceof TdApi.Error) error("Could not sign out. Use Telegram Settings → Devices to revoke this session."); });
    }
}
