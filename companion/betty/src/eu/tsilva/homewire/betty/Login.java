package eu.tsilva.homewire.betty;

/** Preserve password bytes and expose only allowlisted, non-secret login diagnostics. */
final class Login {
    static String input(String value, boolean password) {
        return password ? value : value.trim();
    }

    static String failure(int code, String message) {
        if ("PASSWORD_HASH_INVALID".equals(message))
            return "Telegram rejected the two-step verification password. Check that this is the same account. (PASSWORD_HASH_INVALID)";
        if ("SRP_ID_INVALID".equals(message) || "SRP_PASSWORD_CHANGED".equals(message))
            return "Telegram's password check expired or changed. Close and reopen Betty, then try again. (" + message + ")";
        if (code == 429 || (message != null && message.matches("FLOOD_WAIT_[0-9]+")))
            return "Telegram has limited sign-in attempts. Wait before trying again; changing your password will not remove this limit.";
        if ("PHONE_CODE_INVALID".equals(message))
            return "Telegram rejected the login code. Enter the latest code for this Betty sign-in. (PHONE_CODE_INVALID)";
        if ("PHONE_CODE_EXPIRED".equals(message))
            return "The Telegram login code expired. Reopen Betty to continue sign-in. (PHONE_CODE_EXPIRED)";
        if ("PHONE_NUMBER_INVALID".equals(message))
            return "Check your Telegram phone number, including the country code. (PHONE_NUMBER_INVALID)";
        if (code >= 500)
            return "Telegram could not complete sign-in because of a service or connection error. Try again later.";
        // Never display arbitrary server messages: they may contain account details.
        return "Telegram could not complete this login step (error " + code + ").";
    }

    private Login() {}
}
