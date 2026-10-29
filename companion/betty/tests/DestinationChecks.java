package eu.tsilva.homewire.betty;

import java.util.function.Consumer;
import org.drinkless.tdlib.TdApi;

/** Simulate a user client whose local chat cache is lost on every restart. */
public final class DestinationChecks {
    private static void check(boolean value) {
        if (!value) throw new AssertionError("Destination resolution contract failed");
    }
    private static final class TelegramStub implements Destination.Request {
        TdApi.Chat chat = new TdApi.Chat();
        TdApi.User user = new TdApi.User();
        boolean cached, unavailable;
        int searches;
        TelegramStub() {
            chat.id = 12345; chat.type = new TdApi.ChatTypePrivate(67890);
            user.id = 67890; user.type = new TdApi.UserTypeBot();
        }
        public void send(TdApi.Function<?> query, Consumer<TdApi.Object> result) {
            if (query instanceof TdApi.GetChat) {
                result.accept(cached ? chat : new TdApi.Error(400, "Chat not found"));
            } else if (query instanceof TdApi.SearchPublicChat) {
                check(((TdApi.SearchPublicChat) query).username.equals("SyntheticBettyBot"));
                searches++;
                if (unavailable) result.accept(new TdApi.Error(500, "synthetic-private-server-detail"));
                else { cached = true; result.accept(chat); }
            } else if (query instanceof TdApi.GetUser) {
                check(cached); result.accept(user);
            } else throw new AssertionError("Resolution must not send, reset, start or load message history");
        }
    }
    private static boolean resolves(TelegramStub client, long chat, long user) {
        final boolean[] success = {false}; final String[] failure = {null};
        Destination.resolve(client, "SyntheticBettyBot", chat, user,
            found -> success[0] = found == client.chat,
            error -> { check(!error.contains("synthetic-private-server-detail")); failure[0] = error; });
        check(success[0] != (failure[0] != null));
        return success[0];
    }
    public static void main(String[] args) {
        TelegramStub client = new TelegramStub();
        client.send(new TdApi.GetChat(12345), result -> check(result instanceof TdApi.Error));
        check(resolves(client,12345,67890));
        client.cached = false; // Process termination with chat/message databases disabled.
        check(resolves(client,12345,67890)); check(client.searches == 2);
        client.cached = false; check(resolves(client,0,0)); // Initial selection still resolves.
        check(!resolves(client,12346,67890)); // Username reassigned to another chat.
        check(!resolves(client,12345,67891)); // Bot/user identity mismatch.
        check(!resolves(client,12345,0)); // Incomplete pins cannot select a replacement.
        client.user.type = new TdApi.UserTypeRegular(); check(!resolves(client,12345,67890));
        client.user.type = new TdApi.UserTypeBot(); client.user.id = 67891; check(!resolves(client,12345,67890));
        client.user.id = 67890; client.chat.type = new TdApi.ChatTypeBasicGroup(12345); check(!resolves(client,12345,67890));
        client.chat.type = new TdApi.ChatTypePrivate(67890); client.unavailable = true; check(!resolves(client,12345,67890));
    }
}
