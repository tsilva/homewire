package eu.tsilva.homewire.betty;

import java.util.function.Consumer;
import org.drinkless.tdlib.TdApi;

/** Resolve from Telegram on every start, then enforce the saved identity pins. */
final class Destination {
    interface Request { void send(TdApi.Function<?> query, Consumer<TdApi.Object> result); }

    static void resolve(Request request, String username, long pinnedChat, long pinnedUser,
                        Consumer<TdApi.Chat> found, Consumer<String> failed) {
        if ((pinnedChat == 0) != (pinnedUser == 0)) {
            failed.accept("The saved conversation could not be verified. Its destination has not been changed.");
            return;
        }
        // GetChat is local-only for a user account. The chat database is deliberately
        // disabled, so its cache can be empty after process termination or a reboot.
        request.send(new TdApi.SearchPublicChat(username), object -> {
            if (!(object instanceof TdApi.Chat)) {
                failed.accept("Could not load the conversation from Telegram. Check your connection and reopen Betty.");
                return;
            }
            TdApi.Chat chat = (TdApi.Chat) object;
            if (!(chat.type instanceof TdApi.ChatTypePrivate)) {
                failed.accept("The selected destination is not a private bot conversation.");
                return;
            }
            long userId = ((TdApi.ChatTypePrivate) chat.type).userId;
            if ((pinnedChat != 0 && chat.id != pinnedChat) || (pinnedUser != 0 && userId != pinnedUser)) {
                failed.accept("The bot username no longer matches your saved conversation. The destination has not been changed.");
                return;
            }
            request.send(new TdApi.GetUser(userId), objectUser -> {
                if (!(objectUser instanceof TdApi.User)) {
                    failed.accept("Could not verify the bot with Telegram. Check your connection and reopen Betty.");
                    return;
                }
                TdApi.User user = (TdApi.User) objectUser;
                if (user.id != userId || !(user.type instanceof TdApi.UserTypeBot)) {
                    failed.accept("Bot identity verification failed. The saved destination has not been changed.");
                    return;
                }
                found.accept(chat);
            });
        });
    }
    private Destination() {}
}
