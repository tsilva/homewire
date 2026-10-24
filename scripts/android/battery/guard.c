// Independent root battery guard. Policy adds restrictions to the OEM charger.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/epoll.h>
#include <sys/prctl.h>
#include <sys/signalfd.h>
#include <sys/timerfd.h>
#endif

#ifndef BATTERY_SYSFS
#define BATTERY_SYSFS "/sys/class/power_supply/battery"
#endif
#define SWITCH BATTERY_SYSFS "/input_suspend"

struct sample { int valid, temp, percent, voltage; char health[64], status[64]; };
struct policy { int heat, cold, ceiling, latch, good; };
static const char *root;

static int read_text(const char *path, char *out, size_t size) {
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return -1;
    ssize_t n = read(fd, out, size - 1); close(fd);
    if (n <= 0 || (size_t)n == size - 1) { out[0] = 0; return -1; }
    out[n] = 0;
    while (n && (out[n-1] == '\n' || out[n-1] == '\r')) out[--n] = 0;
    return 0;
}
static int number(const char *path, int *out) {
    char text[64], *end;
    if (read_text(path, text, sizeof(text))) return -1;
    errno = 0; long n = strtol(text, &end, 10);
    if (errno || end == text || *end || n < -100000000 || n > 100000000) return -1;
    *out = (int)n; return 0;
}
static struct sample sample(void) {
    struct sample s = {0}; int present;
    s.valid = !number(BATTERY_SYSFS "/temp", &s.temp)
        && !number(BATTERY_SYSFS "/capacity", &s.percent)
        && !number(BATTERY_SYSFS "/voltage_now", &s.voltage)
        && !number(BATTERY_SYSFS "/present", &present) && present == 1
        && !read_text(BATTERY_SYSFS "/health", s.health, sizeof(s.health))
        && !read_text(BATTERY_SYSFS "/status", s.status, sizeof(s.status))
        && s.temp >= -400 && s.temp <= 1000 && s.percent >= 0 && s.percent <= 100
        && s.voltage >= 2000000 && s.voltage <= 5000000
        && (!strcmp(s.status, "Charging") || !strcmp(s.status, "Discharging")
            || !strcmp(s.status, "Not charging") || !strcmp(s.status, "Full"));
    return s;
}
static int serious(const char *health) {
    const char *names[] = {"Dead", "Over voltage", "Overvoltage", "Unspecified failure",
        "Watchdog timer expire", "Safety timer expire", "Over current", "No battery"};
    for (size_t i = 0; i < sizeof(names)/sizeof(*names); i++) if (!strcmp(health, names[i])) return 1;
    return 0;
}
// All decisions require a new kernel sample. Three good samples span >=60 s.
static int decide(struct policy *p, const struct sample *s) {
    // Evaluate critical fields even when another required field is unreadable.
    if (s->temp >= 500 || serious(s->health) || s->voltage >= 4500000) p->latch = 1;
    if (s->valid) {
        if (s->temp >= 400) p->heat = 1;
        else if (s->temp <= 370) p->heat = 0;
        if (s->temp <= 0) p->cold = 1;
        else if (s->temp >= 50) p->cold = 0;
        if (s->percent >= 70) p->ceiling = 1;
        else if (s->percent <= 50) p->ceiling = 0;
    }
    int safe = s->valid && !p->heat && !p->cold && !p->latch && !strcmp(s->health, "Good");
    p->good = safe ? (p->good < 3 ? p->good + 1 : 3) : 0;
    return safe && !p->ceiling && p->good == 3;
}
static int path(char *out, size_t size, const char *name) {
    int n = snprintf(out, size, "%s/%s", root, name);
    return n < 0 || (size_t)n >= size ? -1 : 0;
}
static int record(const char *name, const char *text) {
    char file[1024], temp[1024];
    if (path(file, sizeof(file), name) || snprintf(temp, sizeof(temp), "%s.new", file) >= (int)sizeof(temp)) return -1;
    int fd = open(temp, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0) return -1;
    size_t length = strlen(text);
    int ok = write(fd, text, length) == (ssize_t)length && !fsync(fd);
    if (close(fd)) ok = 0;
    if (!ok || rename(temp, file)) return -1;
    // Durable attempt/latch before touching the charging switch.
    fd = open(root, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
    if (fd < 0) return -1;
    ok = !fsync(fd); close(fd); return ok ? 0 : -1;
}
static int exists(const char *name) {
    char file[1024]; struct stat st;
    if (path(file, sizeof(file), name)) return 1;
    if (!lstat(file, &st)) return 1;
    return errno != ENOENT;
}
static int remove_record(const char *name) {
    char file[1024]; if (path(file, sizeof(file), name)) return -1;
    return unlink(file) && errno != ENOENT ? -1 : 0;
}
static int switch_to(int enabled) {
    int fd = open(SWITCH, O_WRONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) return -1;
    const char *value = enabled ? "0\n" : "1\n";
    int ok = write(fd, value, 2) == 2;
    if (close(fd)) ok = 0;
    int actual;
    return ok && !number(SWITCH, &actual) && actual == !enabled ? 0 : -1;
}
static int inhibit(void) {
    int saved = exists("held") ? 0 : record("held", "Charging is held by the battery guard.\n");
    int off = switch_to(0); return saved || off ? -1 : 0;
}
static const char *reason(const struct policy *p, const struct sample *s, int failed) {
    if (failed) return "control";
    if (p->latch) return "critical";
    if (!s->valid || p->heat || p->cold || strcmp(s->health, "Good")) return "paused";
    if (s->percent < 15) return "low";
    return "ok";
}
static int cutoff_broken(int settled, int held, int enabled, const char *status) {
    // Durable ownership survives reboot, but the kernel switch does not. The
    // first sample must assert and verify it before testing settled behavior.
    return settled && held && (enabled == 1 || !strcmp(status, "Charging"));
}
static int child(const char *notify, const char *event) {
    pid_t pid = fork(); if (pid < 0) return -1;
    if (!pid) {
        setpgid(0, 0);
        sigset_t mask; sigemptyset(&mask); sigprocmask(SIG_SETMASK, &mask, NULL);
        int nullfd = open("/dev/null", O_RDWR);
        if (nullfd < 0) _exit(126);
        dup2(nullfd, 0); dup2(nullfd, 1); dup2(nullfd, 2); close(nullfd);
        execl(notify, notify, event, (char *)NULL); _exit(127);
    }
    // Bound a failed notification helper; control decisions already completed.
    for (int i = 0; i < 120; i++) {
        int status; pid_t done = waitpid(pid, &status, WNOHANG);
        if (done == pid) return WIFEXITED(status) && !WEXITSTATUS(status) ? 0 : -1;
        if (done < 0 && errno != EINTR) break;
        struct timespec delay = {0, 100000000}; nanosleep(&delay, NULL);
    }
    kill(-pid, SIGKILL); kill(pid, SIGKILL);
    while (waitpid(pid, NULL, 0) < 0 && errno == EINTR) {} return -1;
}
static int setup(void) {
    struct stat st;
    return geteuid() || lstat(root, &st) || !S_ISDIR(st.st_mode) || st.st_uid || (st.st_mode & 077) ? -1 : 0;
}

#ifdef __linux__
static int run(const char *notify) {
    if (setup()) return 1;
    FILE *caps = fopen("/proc/self/status", "r");
    char line[256]; unsigned long long effective = 0;
    if (!caps) { inhibit(); return 1; }
    while (fgets(line, sizeof(line), caps)) if (sscanf(line, "CapEff: %llx", &effective) == 1) break;
    fclose(caps);
    if (!(effective & (1ULL << 35)) || !(effective & (1ULL << 36))) { inhibit(); return 1; }
    sigset_t mask; sigemptyset(&mask); sigaddset(&mask, SIGTERM); sigaddset(&mask, SIGINT);
    if (sigprocmask(SIG_BLOCK, &mask, NULL)) return 1;
    int signals = signalfd(-1, &mask, SFD_CLOEXEC);
    int timer = timerfd_create(CLOCK_BOOTTIME_ALARM, TFD_CLOEXEC);
    int ep = epoll_create1(EPOLL_CLOEXEC);
    if (signals < 0 || timer < 0 || ep < 0) { inhibit(); return 1; }
    struct epoll_event e = {.events = EPOLLIN | EPOLLWAKEUP, .data.fd = timer};
    if (epoll_ctl(ep, EPOLL_CTL_ADD, timer, &e)) { inhibit(); return 1; }
    e.data.fd = signals;
    if (epoll_ctl(ep, EPOLL_CTL_ADD, signals, &e)) { inhibit(); return 1; }
    struct policy p = {.heat = 1, .cold = 1, .ceiling = 1, .latch = exists("latched")};
    int initial;
    if (!exists("held") && !number(SWITCH, &initial) && initial == 1
        && record("external-hold", "Charging was already disabled before guard ownership.\n")) return 1;
    char previous[64] = "ok", delivered[64] = "ok", file[1024];
    if (!path(file, sizeof(file), "event")) read_text(file, previous, sizeof(previous));
    if (!path(file, sizeof(file), "delivered")) read_text(file, delivered, sizeof(delivered));
    int ticks = 0, failed = 0; long long last_send = -1800, last_try = -60;
    for (;;) {
        struct sample s = sample();
        int allowed = decide(&p, &s);
        if (p.latch && !exists("latched") && record("latched", "Manual battery inspection required.\n")) failed = 1;
        int enabled = -1;
        if (number(SWITCH, &enabled) || (enabled != 0 && enabled != 1)) failed = 1;
        else enabled = !enabled;
        int was_held = exists("held");
        if (!allowed || failed || exists("external-hold")) {
            // A settled charging status must agree with a previously verified cutoff.
            int still_charging = cutoff_broken(ticks > 0, was_held, enabled, s.status);
            failed = inhibit() || still_charging || failed;
        } else if (was_held) {
            if (switch_to(1) || remove_record("held")) { failed = 1; inhibit(); }
        }
        const char *event = reason(&p, &s, failed);
        if (!strcmp(event, "ok") && !strcmp(previous, "low") && s.percent < 20) event = "low";
        if (!strcmp(event, "ok") && strcmp(previous, "ok") && p.good < 3) event = previous;
        // Startup hysteresis/charge ceiling is normal and produces no warning.
        if (strcmp(previous, event) && record("event", event)) { inhibit(); return 1; }
        char text[768];
        snprintf(text, sizeof(text), "{\"at\":%lld,\"valid\":%s,\"temperature_c\":%.1f,\"percent\":%d,"
            "\"voltage_v\":%.3f,\"held\":%s,\"latched\":%s,\"event\":\"%s\",\"samples\":%d,\"pid\":%ld}\n",
            (long long)time(NULL), s.valid ? "true" : "false", s.temp / 10.0, s.percent, s.voltage / 1000000.0,
            exists("held") ? "true" : "false", p.latch ? "true" : "false", event, ticks + 1, (long)getpid());
        if (record("status.json", text)) { inhibit(); return 1; }
        struct timespec now; if (clock_gettime(CLOCK_BOOTTIME, &now)) { inhibit(); return 1; }
        long long elapsed = now.tv_sec;
        int active = strcmp(event, "ok") != 0;
        int pending = strcmp(event, delivered) || (active && elapsed - last_send >= 1800);
        if (pending && elapsed - last_try >= 60) {
            last_try = elapsed;
            if (!child(notify, active ? event : "recovery")) {
                if (record("delivered", event)) { inhibit(); return 1; }
                strcpy(delivered, event); last_send = elapsed;
            }
        }
        strcpy(previous, event); ticks++;
        // One wake alarm per 30 seconds, including Android deep suspend.
        struct itimerspec deadline = {.it_value = {.tv_sec = 30}};
        if (timerfd_settime(timer, 0, &deadline, NULL)) { inhibit(); return 1; }
        int n; do { n = epoll_wait(ep, &e, 1, -1); } while (n < 0 && errno == EINTR);
        if (n != 1 || e.data.fd == signals) { inhibit(); return 1; }
        unsigned long long expirations;
        if (read(timer, &expirations, sizeof(expirations)) != sizeof(expirations)) { inhibit(); return 1; }
    }
}
static volatile sig_atomic_t stopping;
static void stop(int sig) { (void)sig; stopping = 1; }
static int supervise(const char *notify) {
    if (setup()) return 1;
    char file[1024]; if (path(file, sizeof(file), "supervisor.lock")) return 1;
    int lock = open(file, O_RDWR | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0600);
    if (lock < 0 || flock(lock, LOCK_EX | LOCK_NB)) return 2;
    struct sigaction handler = {.sa_handler = stop}; sigemptyset(&handler.sa_mask);
    if (sigaction(SIGTERM, &handler, NULL) || sigaction(SIGINT, &handler, NULL)) return 1;
    char pid[64]; snprintf(pid, sizeof(pid), "%ld\n", (long)getpid());
    if (record("supervisor.pid", pid)) { inhibit(); return 1; }
    while (!stopping) {
        pid_t pid = fork();
        if (pid == 0) {
            close(lock); signal(SIGTERM, SIG_DFL); signal(SIGINT, SIG_DFL);
            if (prctl(PR_SET_PDEATHSIG, SIGTERM) || getppid() == 1) _exit(1);
            _exit(run(notify));
        }
        if (pid < 0) { inhibit(); return 1; }
        int status;
        while (waitpid(pid, &status, 0) < 0 && errno == EINTR) if (stopping) kill(pid, SIGKILL);
        inhibit();
        if (!stopping) { struct timespec delay = {5, 0}; nanosleep(&delay, NULL); }
    }
    return 0; // Stopping deliberately leaves charging inhibited.
}
#endif
int main(int argc, char **argv) {
    if (argc == 2 && !strcmp(argv[1], "--sample")) {
        struct sample s = sample();
        printf("{\"valid\":%s,\"temperature_c\":%.1f,\"percent\":%d,\"voltage_v\":%.3f}\n",
            s.valid ? "true" : "false", s.temp / 10.0, s.percent, s.voltage / 1000000.0);
        return !s.valid;
    }
#ifdef __linux__
    if (argc == 4 && !strcmp(argv[1], "--supervise")) { root = argv[2]; return supervise(argv[3]); }
#else
    (void)decide; (void)reason; (void)cutoff_broken; (void)child; (void)setup; (void)inhibit; (void)exists; (void)remove_record;
#endif
    return 2;
}
