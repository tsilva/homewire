// Root performs setup once, then disappears through exec into an unprivileged
// process. No root helper, inherited directory handle or shell enters the jail.
#include <errno.h>
#include <ctype.h>
#include <string.h>
#include <fcntl.h>
#include <grp.h>
#include <linux/capability.h>
#include <limits.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/mount.h>
#include <unistd.h>

static int bind_path(const char *source,const char *target,int recursive,int readonly) {
  if(mount(source,target,NULL,MS_BIND|(recursive?MS_REC:0),NULL))return -1;
  return mount(NULL,target,NULL,MS_REMOUNT|MS_BIND|MS_NOSUID|(readonly?MS_RDONLY:MS_NODEV|MS_NOEXEC),NULL);
}
static int setup(const char *base,const char *agent,char *jail) {
  char target[PATH_MAX],source[PATH_MAX];
  if(snprintf(jail,PATH_MAX,"%s/root",base)>=PATH_MAX)return -1;
  struct stat s;
  if(lstat(base,&s)||!S_ISDIR(s.st_mode)||s.st_uid!=0||(s.st_mode&0077))return -1;
  if(lstat(jail,&s)||!S_ISDIR(s.st_mode)||s.st_uid!=0||(s.st_mode&0022))return -1;
  if(unshare(CLONE_NEWNS)||mount(NULL,"/",NULL,MS_REC|MS_PRIVATE,NULL))return -1;
  if(mount(jail,jail,NULL,MS_BIND,NULL))return -1;
  const char *systems[]={"system","apex"};
  for(unsigned i=0;i<2;i++) {
    snprintf(source,sizeof(source),"/%s",systems[i]);
    snprintf(target,sizeof(target),"%s/%s",jail,systems[i]);
    if(bind_path(source,target,1,1))return -1;
  }
  const char *devices[]={"null","random","urandom","socket/dnsproxyd","socket/fwmarkd"};
  for(unsigned i=0;i<5;i++) {
    snprintf(source,sizeof(source),"/dev/%s",devices[i]);
    snprintf(target,sizeof(target),"%s/dev/%s",jail,devices[i]);
    if(i>=3&&access(source,F_OK))continue;
    if(mount(source,target,NULL,MS_BIND,NULL))return -1;
  }
  snprintf(target,sizeof(target),"%s/dev/__properties__",jail);
  if(bind_path("/dev/__properties__",target,1,1))return -1;
  // Do not share host /proc, including on kernels without PID namespaces.
  if(mount(NULL,jail,NULL,MS_REMOUNT|MS_BIND|MS_RDONLY|MS_NOSUID,NULL))return -1;
  const char *writable[]={"state","tmp"};
  for(unsigned i=0;i<2;i++) {
    snprintf(source,sizeof(source),"%s/%s",base,writable[i]);
    snprintf(target,sizeof(target),"%s/%s",jail,writable[i]);
    if(bind_path(source,target,0,0))return -1;
  }
  const char *policy[]={"config.toml",".secret_key"};
  for(unsigned i=0;i<2;i++) {
    snprintf(source,sizeof(source),"%s/policy/%s",base,policy[i]);
    snprintf(target,sizeof(target),"%s/state/zero/%s",jail,policy[i]);
    if(bind_path(source,target,0,1))return -1;
  }
  snprintf(source,sizeof(source),"%s/policy/TOOLS.md",base);
  snprintf(target,sizeof(target),"%s/state/zero/agents/%s/workspace/TOOLS.md",jail,agent);
  if(bind_path(source,target,0,1))return -1;
  return 0;
}
int main(int argc,char **argv) {
  if(argc<5 || getuid()!=0) return 125;
  char *end=NULL;long uid=strtol(argv[2],&end,10);
  if(!end||*end||uid<10000||uid>99999999||uid%100000<10000||uid%100000>19999)return 125;
  size_t length=strlen(argv[3]);
  if(!length||length>32)return 125;
  for(size_t i=0;i<length;i++)if(!isalnum((unsigned char)argv[3][i])&&argv[3][i]!='_'&&argv[3][i]!='-')return 125;
  char jail[PATH_MAX];
  if(setup(argv[1],argv[3],jail)||chdir(jail)||chroot(".")||chdir("/state/zero")) {
    perror("Sandbox mounts failed");return 125;
  }
  for(int i=0;i<=CAP_LAST_CAP;i++) if(prctl(PR_CAPBSET_DROP,i,0,0,0) && errno!=EINVAL) return 125;
  gid_t groups[]={3003};
  if(setgroups(1,groups) || setgid(uid) || setuid(uid) || prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)) return 125;
  if(prctl(PR_SET_PDEATHSIG,SIGKILL,0,0,0)) return 125;
  struct rlimit core={0,0},files={128,128};
  if(setrlimit(RLIMIT_CORE,&core) || setrlimit(RLIMIT_NOFILE,&files)) return 125;
  // Do not expose host descriptors, credentials, agent env overrides or cwd.
  for(int i=3;i<4096;i++) close(i);
  if(clearenv() || setenv("HOME","/state",1) || setenv("TMPDIR","/tmp",1)
      || setenv("PATH","/bin",1) || setenv("LD_PRELOAD","/bin/guard.so",1)) return 125;
  execv(argv[4],argv+4);
  perror("Sandbox startup failed"); return 125;
}
