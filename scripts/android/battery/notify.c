// Delivery uses private approved text and existing bot credentials, never argv secrets.
#define _GNU_SOURCE
#define _DARWIN_C_SOURCE
#define _POSIX_C_SOURCE 200809L
#include <signal.h>
#include "../telegram_credentials.h"

static int safe_text(const char *text) {
    for (const unsigned char *p = (const unsigned char *)text; *p; p++)
        if (*p < 32 || *p > 126 || *p == '"' || *p == '\\') return 0;
    return *text != 0;
}
static int line(const char *file, char *text, size_t size) {
    int fd = open(file, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return -1;
    ssize_t n = read(fd, text, size-1); close(fd);
    if (n <= 0 || (size_t)n == size-1) return -1;
    text[n] = 0;
    while (n && text[n-1] == '\n') text[--n] = 0;
    return safe_text(text) ? 0 : -1;
}
int main(int argc, char **argv) {
    // config, section, curl, explicit chat file, approved message file.
    if (argc != 6) return 2;
    char secret[1024], chat[64], message[1024];
    if (line(argv[4], chat, sizeof(chat)) || line(argv[5], message, sizeof(message))) return 1;
    for (char *p = chat + (*chat == '-'); *p; p++) if (!isdigit((unsigned char)*p)) return 1;
    if (!isdigit((unsigned char)chat[strlen(chat)-1])) return 1;
    if (token(argv[1], argv[2], secret, sizeof(secret))) return 1;
    int in[2], out[2]; if (pipe(in) || pipe(out)) return 1;
    pid_t pid = fork(); if (pid < 0) return 1;
    if (!pid) {
        dup2(in[0], 0); dup2(out[1], 1);
        int nullfd = open("/dev/null", O_WRONLY); if (nullfd < 0) _exit(126); dup2(nullfd, 2);
        close(in[0]); close(in[1]); close(out[0]); close(out[1]);
        execl(argv[3], argv[3], "--disable", "--config", "-", (char *)NULL); _exit(127);
    }
    signal(SIGPIPE, SIG_IGN);
    close(in[0]); close(out[1]); FILE *stream = fdopen(in[1], "w");
    if (!stream) { kill(pid, SIGKILL); return 1; }
    fprintf(stream, "silent\nfail\nmax-time = 10\nconnect-timeout = 5\nmax-filesize = 4096\n"
        "proto = \"=https\"\nurl = \"https://api.telegram.org/bot%s/sendMessage\"\nrequest = \"POST\"\n"
        "data-urlencode = \"chat_id=%s\"\ndata-urlencode = \"text=%s\"\n", secret, chat, message);
    OPENSSL_cleanse(secret, sizeof(secret)); fclose(stream);
    char result[4097]; size_t used = 0; ssize_t n;
    while (used < sizeof(result)-1 && (n = read(out[0], result+used, sizeof(result)-1-used)) > 0) used += (size_t)n;
    close(out[0]); result[used] = 0;
    if (used == sizeof(result)-1) kill(pid, SIGKILL);
    int status; while (waitpid(pid, &status, 0) < 0) if (errno != EINTR) return 1;
    // Telegram's response begins with ok; don't match quoted message contents.
    char prefix[32]; size_t count = 0;
    for (size_t i = 0; i < used && count < sizeof(prefix)-1; i++)
        if (!isspace((unsigned char)result[i])) prefix[count++] = result[i];
    prefix[count] = 0;
    return WIFEXITED(status) && !WEXITSTATUS(status) && used < sizeof(result)-1
        && !strncmp(prefix, "{\"ok\":true,\"result\":", 20) ? 0 : 1;
}
