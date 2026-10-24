// Discard pending updates for one explicitly enabled Telegram bot. Never send messages.
#define _GNU_SOURCE
#define _DARWIN_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#include "telegram_credentials.h"
static int updates(const char *curl, const char *secret, const char *method, long long offset, char *result, size_t size) {
    int in[2], out[2];
    if (pipe(in) || pipe(out)) return -1;
    pid_t pid = fork();
    if (pid < 0) return -1;
    if (!pid) {
        dup2(in[0], 0); dup2(out[1], 1);
        int nullfd = open("/dev/null", O_WRONLY); if (nullfd < 0) _exit(126); dup2(nullfd, 2);
        close(in[0]); close(in[1]); close(out[0]); close(out[1]);
        execl(curl, curl, "--disable", "--config", "-", (char *)NULL); _exit(127);
    }
    close(in[0]); close(out[1]);
    FILE *stream = fdopen(in[1], "w");
    if (!stream) return -1;
    // Credentials travel through an anonymous pipe, never argv, logs or disk.
    fprintf(stream, "silent\nfail\nmax-time = 10\nconnect-timeout = 5\nmax-filesize = 65536\nurl = \"https://api.telegram.org/bot%s/%s\"\nrequest = \"POST\"\ndata = \"offset=%lld&limit=1&timeout=0\"\n", secret, method, offset);
    fclose(stream);
    size_t used = 0; ssize_t n;
    while (used < size-1 && (n = read(out[0], result+used, size-1-used)) > 0) used += (size_t)n;
    result[used] = 0; close(out[0]);
    int status;
    while (waitpid(pid, &status, 0) < 0) if (errno != EINTR) return -1;
    if (!WIFEXITED(status) || WEXITSTATUS(status) || used == size-1) {
        fprintf(stderr, "Telegram transport failed (exit=%d, bounded=%s)\n", WIFEXITED(status) ? WEXITSTATUS(status) : -1, used == size-1 ? "false" : "true"); return -1;
    }
    // Normalize whitespace outside JSON strings for structural field checks.
    int quoted = 0, escape = 0; char *write = result;
    for (char *p = result; *p; p++) {
        if (!quoted && isspace((unsigned char)*p)) continue;
        *write++ = *p;
        if (escape) escape = 0;
        else if (*p == '\\' && quoted) escape = 1;
        else if (*p == '"') quoted = !quoted;
    }
    *write = 0;
    return !strncmp(result, "{\"ok\":true,\"result\":", 20) ? 0 : -1;
}
int main(int argc, char **argv) {
    if (argc != 4 && argc != 5) return 2;
    char secret[1024], result[65537];
    if (token(argv[1], argv[2], secret, sizeof(secret))) return 1;
    if (argc == 5) {
        if (!strcmp(argv[4], "--check")) return 0;
        if (strcmp(argv[4], "--check-network")) return 2;
        if (updates(argv[3], secret, "getMe", 0, result, sizeof(result))) return 1;
        return strstr(result, "\"is_bot\":true") ? 0 : 1;
    }
    if (updates(argv[3], secret, "getUpdates", -1, result, sizeof(result))) return 1;
    if (!strcmp(result, "{\"ok\":true,\"result\":[]}")) return 0;
    char *id = strstr(result, "\"update_id\":");
    if (!id) return 1;
    char *end; errno = 0; long long last = strtoll(id+12, &end, 10);
    if (errno || last < 0 || last > 2147483647LL || (*end != ',' && *end != '}')) return 1;
    // A positive next offset confirms and forgets the final queued update.
    if (updates(argv[3], secret, "getUpdates", last+1, result, sizeof(result))) return 1;
    memset(secret, 0, sizeof(secret));
    return strcmp(result, "{\"ok\":true,\"result\":[]}") ? 1 : 0;
}
