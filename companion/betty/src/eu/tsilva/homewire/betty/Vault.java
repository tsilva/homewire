package eu.tsilva.homewire.betty;

import android.content.Context;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.AtomicFile;
import org.json.JSONObject;
import java.io.File;
import java.io.FileOutputStream;
import java.security.KeyStore;
import java.security.SecureRandom;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** A device-bound encryption key protects configuration and TDLib's database key. */
final class Vault {
    private static final String ALIAS = "betty-session-v1";
    private final AtomicFile file;
    private final SecretKey key;

    Vault(Context context) throws Exception {
        file = new AtomicFile(new File(context.getNoBackupFilesDir(), "session.sealed"));
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (!store.containsAlias(ALIAS)) {
            KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            generator.init(new KeyGenParameterSpec.Builder(ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setKeySize(256).build());
            generator.generateKey();
        }
        key = (SecretKey) store.getKey(ALIAS, null);
    }

    JSONObject read() throws Exception {
        if (!file.getBaseFile().exists()) return new JSONObject();
        byte[] data = file.readFully();
        if (data.length < 28 || data.length > 16384) throw new IllegalStateException("Invalid session storage");
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, Arrays.copyOf(data, 12)));
        byte[] plain = cipher.doFinal(data, 12, data.length - 12);
        try { return new JSONObject(new String(plain, java.nio.charset.StandardCharsets.UTF_8)); }
        finally { Arrays.fill(plain, (byte) 0); }
    }

    void write(JSONObject value) throws Exception {
        byte[] plain = value.toString().getBytes(java.nio.charset.StandardCharsets.UTF_8);
        if (plain.length > 8192) throw new IllegalArgumentException("Oversized session storage");
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.ENCRYPT_MODE, key);
        byte[] encrypted;
        try { encrypted = cipher.doFinal(plain); }
        finally { Arrays.fill(plain, (byte) 0); }
        FileOutputStream stream = file.startWrite();
        try {
            stream.write(cipher.getIV()); stream.write(encrypted); file.finishWrite(stream);
        } catch (Exception failure) { file.failWrite(stream); throw failure; }
    }

    static byte[] randomKey() { byte[] key = new byte[32]; new SecureRandom().nextBytes(key); return key; }
}
