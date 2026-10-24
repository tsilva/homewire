// Run through the normal unprivileged launcher, guard and read-only root.
// Operator-created symlinks are negative fixtures, never followed/read here.
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int resolves(const char *path) {
  char buffer[PATH_MAX];
  char *allocated = realpath(path, NULL);
  int ok = allocated && !strcmp(allocated, path);
  if (!ok) printf("dashboard_canonicalization_errno=%d\n", errno);
  free(allocated);
  if (!ok) return 0;
  char *provided = realpath(path, buffer);
  if (!provided) printf("dashboard_buffer_errno=%d\n", errno);
  return provided == buffer && !strcmp(buffer, path);
}

static int rejects(const char *path) {
  char *resolved = realpath(path, NULL);
  int denied = resolved == NULL;
  free(resolved);
  return denied;
}

int main(void) {
  struct stat s;
  int proc_hidden = access("/proc/self/fd", F_OK) < 0 && errno == ENOENT &&
                    access("/proc/1", F_OK) < 0;
  int symlinks = lstat("/bin/web/.compat-probe-link", &s) == 0 && S_ISLNK(s.st_mode) &&
                 lstat("/bin/web/.compat-probe-dir", &s) == 0 && S_ISLNK(s.st_mode);
  int web = resolves("/bin/web") && resolves("/bin/web/index.html");
  int contained = symlinks &&
      rejects("/bin/web/.compat-probe-link") &&
      rejects("/bin/web/.compat-probe-dir/config.toml") &&
      rejects("/bin/web/../guard.so") &&
      rejects("/bin/web/./index.html") &&
      rejects("/bin/web//index.html") &&
      rejects("/bin/web/") &&
      rejects("/bin/web/does-not-exist") &&
      rejects("/bin/web-other/index.html") &&
      rejects("/state/zero/config.toml");
  int fd = open("/bin/web/index.html", O_WRONLY);
  int readonly = fd < 0;
  if (fd >= 0) close(fd);
  printf("dashboard_paths=%s path_escape_denied=%s proc_hidden=%s assets_readonly=%s\n",
         web ? "true" : "false", contained ? "true" : "false",
         proc_hidden ? "true" : "false", readonly ? "true" : "false");
  return web && contained && proc_hidden && readonly ? 0 : 1;
}
