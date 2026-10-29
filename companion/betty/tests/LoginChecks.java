package eu.tsilva.homewire.betty;

/** Synthetic credentials only; this check never connects to Telegram. */
public final class LoginChecks {
    private static void check(boolean value) {
        if (!value) throw new AssertionError("Login contract failed");
    }
    public static void main(String[] args) {
        String password = " \tSynthetic p\u00e4ss\ud83d\udd12\u0301\n ";
        check(Login.input(password, true).equals(password));
        check(Login.input("   ", true).equals("   "));
        check(Login.input("  123456  ", false).equals("123456"));
        check(Login.failure(400, "PASSWORD_HASH_INVALID").contains("PASSWORD_HASH_INVALID"));
        check(Login.failure(400, "SRP_ID_INVALID").contains("expired"));
        check(Login.failure(400, "SRP_PASSWORD_CHANGED").contains("changed"));
        check(Login.failure(429, "Too Many Requests: retry later").contains("limited"));
        check(Login.failure(420, "FLOOD_WAIT_60").contains("limited"));
        check(Login.failure(400, "PHONE_CODE_INVALID").contains("latest code"));
        check(Login.failure(400, "PHONE_CODE_EXPIRED").contains("expired"));
        check(Login.failure(500, "synthetic-private-detail").contains("service"));
        check(!Login.failure(400, "synthetic-private-detail").contains("synthetic-private-detail"));
        check(!Login.failure(400, "FLOOD_WAIT_60 synthetic-private-detail").contains("synthetic-private-detail"));
        check(Login.failure(400, null).contains("error 400"));
    }
}
