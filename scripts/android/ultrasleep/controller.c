// Root-only, event-driven Android sleep controller. No device or agent controls.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int plan(time_t now, int start, int end, time_t *next) {
    struct tm local;
    if (!localtime_r(&now, &local)) return -1;
    int minute = local.tm_hour * 60 + local.tm_min;
    int asleep = start == 0 ? minute < end : minute >= start || minute < end;
    if ((asleep ? end : start) <= minute) local.tm_mday++;
    local.tm_hour = (asleep ? end : start) / 60;
    local.tm_min = (asleep ? end : start) % 60;
    local.tm_sec = 0;
    local.tm_isdst = -1;
    *next = mktime(&local);
    return *next > now ? asleep : -1;
}
static int number(const char *s, int low, int high) {
    char *end; errno = 0; long n = strtol(s, &end, 10);
    return errno || !*s || *end || n < low || n > high ? -1 : (int)n;
}
static int valid_window(int start, int end) {
    return start >= 0 && end >= 0 && start != end && (start == 0 || start > end);
}
int main(int argc, char **argv);
#ifdef __linux__
#include <stdint.h>
#include <sys/epoll.h>
#include <sys/file.h>
#include <sys/signalfd.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <signal.h>

static const char *root, *action;
static int timer_fd, event_fd, signal_fd;
static double clock_seconds(clockid_t c) {
    struct timespec t; if (clock_gettime(c, &t)) return -1;
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}
static int write_record(const char *name, const char *value) {
    char path[512], temp[512];
    if (snprintf(path, sizeof(path), "%s/%s", root, name) >= (int)sizeof(path) ||
        snprintf(temp, sizeof(temp), "%s/%s.new", root, name) >= (int)sizeof(temp)) return -1;
    int fd = open(temp, O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW|O_CLOEXEC, 0600);
    if (fd < 0) return -1;
    size_t n = strlen(value);
    int ok = write(fd, value, n) == (ssize_t)n && fsync(fd) == 0;
    if (close(fd)) ok = 0;
    if (ok && rename(temp, path) == 0) return 0;
    unlink(temp); return -1;
}
static int arm(time_t value, int absolute) {
    struct itimerspec t = {.it_value = {.tv_sec = value}};
    return timerfd_settime(timer_fd, absolute ? TFD_TIMER_ABSTIME|TFD_TIMER_CANCEL_ON_SET : 0, &t, NULL);
}
static int wait_event(void) {
    struct epoll_event event;
    int n;
    do { n = epoll_wait(event_fd, &event, 1, -1); } while (n < 0 && errno == EINTR);
    if (n < 0) return -1;
    if (event.data.fd == signal_fd) {
        struct signalfd_siginfo s;
        if (read(signal_fd, &s, sizeof(s)) != sizeof(s)) return -1;
        return s.ssi_signo == SIGUSR1 ? 2 : 3;
    }
    uint64_t count;
    if (read(timer_fd, &count, sizeof(count)) == sizeof(count)) return 0;
    return errno == ECANCELED ? 1 : -1;
}
static int transition(const char *mode) {
    pid_t child = fork();
    if (child < 0) return -1;
    if (!child) {
        sigset_t empty; sigemptyset(&empty); sigprocmask(SIG_SETMASK, &empty, NULL);
        // Android Binder shell calls reject root-private log file descriptors.
        int nullfd = open("/dev/null", O_RDWR);
        if (nullfd < 0 || dup2(nullfd, 0) < 0 || dup2(nullfd, 1) < 0 || dup2(nullfd, 2) < 0) _exit(126);
        if (nullfd > 2) close(nullfd);
        execl("/system/bin/timeout", "timeout", "-k", "5", "90", "/system/bin/sh", action, mode, (char *)NULL);
        _exit(127);
    }
    int status;
    while (waitpid(child, &status, 0) < 0) if (errno != EINTR) return -1;
    return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -1;
}
static int setup(int test_clock) {
    struct stat st;
    if (geteuid() || lstat(root, &st) || !S_ISDIR(st.st_mode) || st.st_uid || (st.st_mode & 077)) return -1;
    if (lstat(action, &st) || !S_ISREG(st.st_mode) || st.st_uid || (st.st_mode & 022)) return -1;
    // EPOLLWAKEUP needs CAP_BLOCK_SUSPEND; the alarm needs CAP_WAKE_ALARM.
    FILE *f = fopen("/proc/self/status", "re"); unsigned long long caps = 0; char line[256];
    if (!f) return -1;
    while (fgets(line, sizeof(line), f)) if (sscanf(line, "CapEff: %llx", &caps) == 1) break;
    fclose(f);
    if (!(caps & (1ULL<<35)) || !(caps & (1ULL<<36))) return -1;
    char path[512]; snprintf(path, sizeof(path), "%s/controller.lock", root);
    int lock = open(path, O_RDWR|O_CREAT|O_NOFOLLOW|O_CLOEXEC, 0600);
    if (lock < 0 || flock(lock, LOCK_EX|LOCK_NB)) return -1;
    char pid[64]; snprintf(pid, sizeof(pid), "%ld\n", (long)getpid());
    if (write_record("controller.pid", pid)) return -1;
    sigset_t mask; sigemptyset(&mask); sigaddset(&mask, SIGTERM); sigaddset(&mask, SIGINT); sigaddset(&mask, SIGUSR1);
    if (sigprocmask(SIG_BLOCK, &mask, NULL)) return -1;
    signal_fd = signalfd(-1, &mask, SFD_CLOEXEC);
    timer_fd = timerfd_create(test_clock ? CLOCK_BOOTTIME_ALARM : CLOCK_REALTIME_ALARM, TFD_CLOEXEC|TFD_NONBLOCK);
    event_fd = epoll_create1(EPOLL_CLOEXEC);
    if (signal_fd < 0 || timer_fd < 0 || event_fd < 0) return -1;
    struct epoll_event e = {.events = EPOLLIN|EPOLLWAKEUP};
    e.data.fd = timer_fd;
    if (epoll_ctl(event_fd, EPOLL_CTL_ADD, timer_fd, &e)) return -1;
    e.data.fd = signal_fd;
    return epoll_ctl(event_fd, EPOLL_CTL_ADD, signal_fd, &e);
}
static int status(const char *phase, time_t next, double suspended, int ok) {
    char text[512];
    snprintf(text, sizeof(text), "{\"phase\":\"%s\",\"at\":%lld,\"next_transition\":%lld,\"suspended_seconds\":%.3f,\"ok\":%s,\"action\":\"%s\"}\n",
        phase, (long long)time(NULL), (long long)next, suspended, ok ? "true" : "false", action);
    return write_record("status.json", text);
}
static int supervise(int argc, char **argv) {
    root = argv[2]; action = argv[3];
    struct stat st;
    if (geteuid() || lstat(root, &st) || !S_ISDIR(st.st_mode) || st.st_uid || (st.st_mode & 077)) return 2;
    char path[512];
    if (snprintf(path, sizeof(path), "%s/supervisor-v2.lock", root) >= (int)sizeof(path)) return 2;
    int lock = open(path, O_RDWR|O_CREAT|O_NOFOLLOW|O_CLOEXEC, 0600);
    if (lock < 0 || flock(lock, LOCK_EX|LOCK_NB)) return 2;
    pid_t child = fork();
    if (child < 0) return 1;
    if (!child) {
        close(lock);
        char *args[10] = {argv[0], argv[4], argv[2], argv[3]};
        for (int i = 5; i < argc; i++) args[i-1] = argv[i];
        args[argc-1] = NULL;
        _exit(main(argc-1, args));
    }
    int result;
    while (waitpid(child, &result, 0) < 0) if (errno != EINTR) return 1;
    int code = WIFEXITED(result) ? WEXITSTATUS(result) : 1;
    // A duplicate or invalid controller never changes state and must not wake an owner.
    if (code != 2 && transition("leave")) return 1;
    return code;
}
static int run_test(int seconds, int probe) {
    if (arm(1, 0) || wait_event() != 0) return 1;
    if (probe) { if (transition("check")) return 1; puts("wake_alarm_and_suspend_capabilities=true"); return 0; }
    if (write_record("session", "test\n")) return 1;
    if (arm(seconds + 100, 0) || status("entering", time(NULL)+seconds, 0, 1)) return 1;
    if (transition("enter")) { transition("leave"); status("entry_failed", 0, 0, 0); return 1; }
    double boot = clock_seconds(CLOCK_BOOTTIME), mono = clock_seconds(CLOCK_MONOTONIC);
    if (arm(seconds, 0) || status("asleep", time(NULL)+seconds, 0, 1)) { transition("leave"); return 1; }
    int event = wait_event();
    double suspended = (clock_seconds(CLOCK_BOOTTIME)-boot) - (clock_seconds(CLOCK_MONOTONIC)-mono);
    if (suspended < 0) suspended = 0;
    int ok = transition("leave") == 0 && event == 0;
    status(ok ? "test_complete" : "test_interrupted", 0, suspended, ok);
    return ok ? 0 : 1;
}
static int run_schedule(int start, int end) {
    int sleeping = 0; time_t override = 0;
    if (write_record("session", "nightly\n")) return 1;
    // Recover a persisted transaction before accepting a new schedule.
    if (arm(1, 0) || wait_event() != 0 || transition("leave")) return 1;
    for (;;) {
        time_t now = time(NULL), next; int target = plan(now, start, end, &next);
        if (target < 0) return 1;
        if (now < override) { target = 0; if (plan(override, start, end, &next) < 0) return 1; }
        if (target != sleeping) {
            if (arm(next, 1)) return 1; // Recovery alarm is armed before changing services.
            if (transition(target ? "enter" : "leave")) { transition("leave"); status("transition_failed", 0, 0, 0); return 1; }
            sleeping = target;
        }
        if (status(sleeping ? "asleep" : "awake", next, 0, 1) || arm(next, 1)) return 1;
        int event = wait_event();
        if (event < 0 || event == 3) { int ok = transition("leave") == 0; status("stopped", 0, 0, ok); return ok ? 0 : 1; }
        if (event == 2) { // Operator wake bypasses only the current night.
            if (transition("leave")) return 1;
            sleeping = 0;
            time_t wake_next;
            int night = plan(time(NULL), start, end, &wake_next);
            if (night < 0) return 1;
            override = night ? wake_next : 0;
        }
    }
}
#endif
int main(int argc, char **argv) {
    if (argc == 6 && !strcmp(argv[1], "--plan")) {
        int start = number(argv[4], 0, 1439), end = number(argv[5], 0, 1438); char *p;
        time_t now = (time_t)strtoll(argv[3], &p, 10), next;
        if (*p || !valid_window(start, end) || setenv("TZ", argv[2], 1)) return 2;
        tzset(); int asleep = plan(now, start, end, &next);
        if (asleep < 0) return 2;
        printf("{\"asleep\":%s,\"next_transition\":%lld}\n", asleep ? "true" : "false", (long long)next); return 0;
    }
#ifdef __linux__
    if ((argc == 6 || argc == 8) && !strcmp(argv[1], "--supervise")) return supervise(argc, argv);
    if (argc == 7 && !strcmp(argv[1], "--run")) {
        root = argv[2]; action = argv[3]; int start = number(argv[5], 0, 1439), end = number(argv[6], 0, 1438);
        if (!valid_window(start, end) || setenv("TZ", argv[4], 1) || setup(0)) return 2;
        tzset(); return run_schedule(start, end);
    }
    if (argc == 5 && (!strcmp(argv[1], "--test") || !strcmp(argv[1], "--probe"))) {
        root = argv[2]; action = argv[3]; int seconds = number(argv[4], 1, 3600);
        if (seconds < 0 || setup(1)) return 2;
        return run_test(seconds, !strcmp(argv[1], "--probe"));
    }
#endif
    fputs("Invalid ultrasleep controller arguments\n", stderr); return 2;
}
