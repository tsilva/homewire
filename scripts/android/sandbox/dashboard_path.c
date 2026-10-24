// Android's realpath() needs /proc/self/fd, which is intentionally absent in
// this jail. Preserve libc behavior, with a narrow fallback for immutable web
// assets. This does not expose procfs or change any process/filesystem policy.
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

static char *(*libc_realpath)(const char *, char *);
static pthread_once_t resolve_once = PTHREAD_ONCE_INIT;

static void resolve_libc(void) {
  libc_realpath = (char *(*)(const char *, char *))dlsym(RTLD_NEXT, "realpath");
}

static int immutable(int fd) {
  struct stat s;
  struct statvfs fs;
  if (fstat(fd, &s) || fstatvfs(fd, &fs)) return 0;
  if (!(fs.f_flag & ST_RDONLY) || s.st_uid != 0 || (s.st_mode & 0022) ||
      (!S_ISDIR(s.st_mode) && !S_ISREG(s.st_mode))) {
    errno = EACCES;
    return 0;
  }
  return 1;
}

static char *dashboard_realpath(const char *path, char *result) {
  static const char root[] = "/bin/web";
  size_t length = strnlen(path, PATH_MAX);
  if (length >= PATH_MAX) { errno = ENAMETOOLONG; return NULL; }
  if (strncmp(path, root, sizeof(root) - 1) ||
      (path[sizeof(root) - 1] && path[sizeof(root) - 1] != '/')) {
    errno = ENOENT;
    return NULL;
  }
  // Do not normalize user input: reject aliases, traversal, empty components
  // and every symlink. Walk from / using directory descriptors, never cwd.
  char components[PATH_MAX];
  memcpy(components, path + 1, length);
  int fd = open("/", O_RDONLY | O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
  if (fd < 0) return NULL;
  if (!immutable(fd)) { int e = errno; close(fd); errno = e; return NULL; }
  char *component = components;
  for (;;) {
    char *slash = strchr(component, '/');
    if (slash) *slash = '\0';
    if (!component[0] || !strcmp(component, ".") || !strcmp(component, "..")) {
      close(fd); errno = EINVAL; return NULL;
    }
    int flags = O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK;
    if (slash) flags |= O_DIRECTORY;
    int next = openat(fd, component, flags);
    int error = errno;
    close(fd);
    if (next < 0) { errno = error; return NULL; }
    fd = next;
    if (!immutable(fd)) { error = errno; close(fd); errno = error; return NULL; }
    if (!slash) break;
    component = slash + 1;
  }
  close(fd);
  if (result) return memcpy(result, path, length + 1);
  return strdup(path);
}

char *realpath(const char *path, char *result) {
  pthread_once(&resolve_once, resolve_libc);
  if (!libc_realpath) { errno = ENOSYS; return NULL; }
  char *resolved = libc_realpath(path, result);
  if (resolved || !path || errno != ENOENT) return resolved;
  int saved = errno;
  // A working procfs, or a permission failure, must retain libc's result.
  if (access("/proc/self/fd", F_OK) == 0 || errno != ENOENT) {
    errno = saved;
    return NULL;
  }
  return dashboard_realpath(path, result);
}
