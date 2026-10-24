package eu.tsilva.homewire.tv;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import java.math.BigInteger;
import java.security.KeyPairGenerator;
import java.security.KeyStore;
import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.Calendar;
import javax.net.ssl.X509ExtendedKeyManager;
import javax.net.ssl.SSLContext;
import javax.security.auth.x500.X500Principal;

final class Credentials {
    private static final String ALIAS = "homewire_tv_tls_v1";
    private static KeyStore keys() throws Exception {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (!store.containsAlias(ALIAS)) {
            Calendar end = Calendar.getInstance(); end.add(Calendar.YEAR, 10);
            KeyPairGenerator generator = KeyPairGenerator.getInstance("RSA", "AndroidKeyStore");
            generator.initialize(new KeyGenParameterSpec.Builder(ALIAS,
                KeyProperties.PURPOSE_SIGN | KeyProperties.PURPOSE_VERIFY)
                .setKeySize(2048).setDigests(KeyProperties.DIGEST_NONE, KeyProperties.DIGEST_SHA256, KeyProperties.DIGEST_SHA384,
                    KeyProperties.DIGEST_SHA512)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE,
                    KeyProperties.ENCRYPTION_PADDING_RSA_PKCS1)
                .setSignaturePaddings(KeyProperties.SIGNATURE_PADDING_RSA_PKCS1,
                    KeyProperties.SIGNATURE_PADDING_RSA_PSS)
                .setCertificateSubject(new X500Principal("CN=Homewire TV"))
                .setCertificateSerialNumber(new BigInteger(128, new SecureRandom()).setBit(0))
                .setCertificateNotAfter(end.getTime()).build());
            generator.generateKeyPair();
        }
        return store;
    }
    static String pin() throws Exception {
        byte[] bytes = MessageDigest.getInstance("SHA-256")
            .digest(keys().getCertificate(ALIAS).getEncoded());
        StringBuilder hex = new StringBuilder();
        for (byte b : bytes) hex.append(String.format(java.util.Locale.ROOT, "%02x", b & 255));
        return hex.toString();
    }
    static SSLContext tls() throws Exception {
        KeyStore store = keys();
        X509ExtendedKeyManager manager = new X509ExtendedKeyManager() {
            public String chooseServerAlias(String type, java.security.Principal[] issuers, java.net.Socket socket) {
                return "RSA".equals(type) ? ALIAS : null;
            }
            public String chooseEngineServerAlias(String type, java.security.Principal[] issuers, javax.net.ssl.SSLEngine engine) {
                return "RSA".equals(type) ? ALIAS : null;
            }
            public String[] getServerAliases(String type, java.security.Principal[] issuers) {
                return "RSA".equals(type) ? new String[]{ALIAS} : null;
            }
            public String chooseClientAlias(String[] types, java.security.Principal[] issuers, java.net.Socket socket) { return null; }
            public String[] getClientAliases(String type, java.security.Principal[] issuers) { return null; }
            public java.security.PrivateKey getPrivateKey(String alias) {
                try { return ALIAS.equals(alias) ? (java.security.PrivateKey) store.getKey(ALIAS, null) : null; }
                catch (Exception e) { return null; }
            }
            public java.security.cert.X509Certificate[] getCertificateChain(String alias) {
                try { return ALIAS.equals(alias) ? new java.security.cert.X509Certificate[]{
                    (java.security.cert.X509Certificate) store.getCertificate(ALIAS)} : null; }
                catch (Exception e) { return null; }
            }
        };
        SSLContext context = SSLContext.getInstance("TLS");
        context.init(new javax.net.ssl.KeyManager[]{manager}, null, new SecureRandom());
        return context;
    }
}
