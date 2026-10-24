// Shared reader for one explicitly enabled, existing encrypted bot credential.
#ifndef HOMEWIRE_TELEGRAM_CREDENTIALS_H
#define HOMEWIRE_TELEGRAM_CREDENTIALS_H
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#include <fcntl.h>
#include <openssl/evp.h>
#include <openssl/crypto.h>

static char *trim(char *s) {
    while (isspace((unsigned char)*s)) s++;
    char *end = s + strlen(s);
    while (end > s && isspace((unsigned char)end[-1])) *--end = 0;
    return s;
}
static int hex(const char *s, unsigned char *out, size_t capacity) {
    size_t n = strlen(s); if (n % 2 || n/2 > capacity) return -1;
    for (size_t i = 0; i < n; i += 2) {
        if (!isxdigit((unsigned char)s[i]) || !isxdigit((unsigned char)s[i+1])) return -1;
        char pair[3] = {s[i], s[i+1], 0}; out[i/2] = (unsigned char)strtoul(pair, NULL, 16);
    }
    return (int)(n/2);
}
static int decrypt(const char *config, char *value, size_t capacity) {
    if (strncmp(value, "enc2:", 5)) return 0;
    char keyfile[1024], keytext[80];
    if (strlen(config) >= sizeof(keyfile)) return -1;
    strcpy(keyfile, config); char *base = strrchr(keyfile, '/'); if (!base) return -1;
    if ((size_t)(base-keyfile)+13 >= sizeof(keyfile)) return -1;
    strcpy(base+1, ".secret_key");
    int fd = open(keyfile, O_RDONLY|O_NOFOLLOW); if (fd < 0) return -1;
    ssize_t n = read(fd, keytext, sizeof(keytext)-1); close(fd);
    if (n < 0) return -1;
    keytext[n] = 0;
    unsigned char key[32], blob[512], plain[512];
    int length = hex(value+5, blob, sizeof(blob));
    if (hex(trim(keytext), key, sizeof(key)) != 32 || length < 29) return -1;
    EVP_CIPHER_CTX *ctx = EVP_CIPHER_CTX_new(); int used = 0, tail = 0, ok = 0;
    if (ctx && EVP_DecryptInit_ex(ctx, EVP_chacha20_poly1305(), NULL, NULL, NULL) == 1 &&
        EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_AEAD_SET_IVLEN, 12, NULL) == 1 &&
        EVP_DecryptInit_ex(ctx, NULL, NULL, key, blob) == 1 &&
        EVP_DecryptUpdate(ctx, plain, &used, blob+12, length-28) == 1 &&
        EVP_CIPHER_CTX_ctrl(ctx, EVP_CTRL_AEAD_SET_TAG, 16, blob+length-16) == 1 &&
        EVP_DecryptFinal_ex(ctx, plain+used, &tail) == 1 && (size_t)(used+tail) < capacity) {
        plain[used+tail] = 0;
        if (strlen((char *)plain) == (size_t)(used+tail)) { strcpy(value, (char *)plain); ok = 1; }
    }
    EVP_CIPHER_CTX_free(ctx); OPENSSL_cleanse(key, sizeof(key));
    OPENSSL_cleanse(keytext, sizeof(keytext)); OPENSSL_cleanse(plain, sizeof(plain));
    return ok ? 0 : -1;
}
static int token(const char *file, const char *section, char *out, size_t size) {
    FILE *f = fopen(file, "r");
    if (!f) return -1;
    char line[4096]; int active = 0, enabled = 0, found = 0;
    while (fgets(line, sizeof(line), f)) {
        char *s = trim(line);
        if (*s == '[') {
            char canonical[4096]; size_t n = 0;
            for (char *p = s; *p; p++) {
                if (*p == '"' || *p == '\'' || isspace((unsigned char)*p)) continue;
                canonical[n++] = *p;
            }
            canonical[n] = 0; active = !strcmp(canonical, section); continue;
        }
        if (!active) continue;
        char *eq = strchr(s, '=');
        if (!eq) continue;
        *eq++ = 0; s = trim(s); eq = trim(eq);
        size_t keylen = strlen(s);
        if (keylen >= 2 && (s[0] == '"' || s[0] == '\'') && s[keylen-1] == s[0]) { s[keylen-1] = 0; s++; }
        if (!strcmp(s, "enabled")) enabled = !strcmp(eq, "true");
        if (!strcmp(s, "bot_token")) {
            size_t n = strlen(eq);
            if (n < 4 || (eq[0] != '"' && eq[0] != '\'') || eq[n-1] != eq[0] || n-2 >= size) { fclose(f); return -1; }
            eq[n-1] = 0; strcpy(out, eq+1); found++;
        }
    }
    fclose(f);
    if (!enabled || found != 1) return -1;
    if (decrypt(file, out, size)) return -1;
    int colon = 0;
    for (char *p = out; *p; p++) {
        if (*p == ':') colon++;
        else if (!colon && !isdigit((unsigned char)*p)) return -1;
        else if (!isalnum((unsigned char)*p) && *p != '-' && *p != '_') return -1;
    }
    return colon == 1 ? 0 : -1;
}
#endif
