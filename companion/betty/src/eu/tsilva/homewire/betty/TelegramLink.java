package eu.tsilva.homewire.betty;

/** Opens a verified bot conversation without a draft, /start command or second send. */
final class TelegramLink {
    static String chat(String username) {
        if(!username.matches("[a-zA-Z][a-zA-Z0-9_]{4,31}"))throw new IllegalArgumentException("Invalid username");
        return "tg://resolve?domain="+username;
    }
    private TelegramLink() {}
}
