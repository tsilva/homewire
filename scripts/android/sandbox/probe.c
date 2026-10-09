// Negative checks share the exact launcher, namespace and seccomp library.
#include <errno.h>
#include <stdlib.h>
#include <limits.h>
#include <fcntl.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <poll.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/prctl.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>
static int denied(const char *path,int flags) {
  int fd=open(path,flags); if(fd>=0) {close(fd);return 0;}return 1;
}
static int connects(const char *address,int port) {
  int s=socket(AF_INET,SOCK_STREAM|SOCK_NONBLOCK,0);if(s<0)return -1;
  struct sockaddr_in a={.sin_family=AF_INET,.sin_port=htons(port)};
  inet_pton(AF_INET,address,&a.sin_addr);
  int rc=connect(s,(struct sockaddr*)&a,sizeof(a));
  if(rc<0&&errno==EINPROGRESS) {
    struct pollfd p={.fd=s,.events=POLLOUT};
    int e=0;socklen_t n=sizeof(e);
    rc=poll(&p,1,1000)>0&&getsockopt(s,SOL_SOCKET,SO_ERROR,&e,&n)==0&&e==0?0:-1;
  }
  close(s);return rc==0;
}
int main(int argc,char **argv) {
  if(argc!=5 && argc!=10)return 125;
  char path[PATH_MAX],proc_path[PATH_MAX];
  if(snprintf(path,sizeof(path),"/state/zero/agents/%s/workspace/TOOLS.md",argv[1])>=(int)sizeof(path))return 125;
  if(snprintf(proc_path,sizeof(proc_path),"/proc/1/root%s",argv[2])>=(int)sizeof(proc_path))return 125;
  int ok=getuid()%100000>=10000 && getuid()%100000<20000 && prctl(PR_GET_NO_NEW_PRIVS,0,0,0,0)==1
      && prctl(PR_GET_SECCOMP,0,0,0,0)==2;
  ok &= denied("/state/zero/config.toml",O_WRONLY);
  ok &= denied("/state/zero/.secret_key",O_WRONLY);
  ok &= denied(path,O_WRONLY);
  ok &= denied(argv[2],O_RDONLY);
  ok &= denied(proc_path,O_RDONLY);
  ok &= denied(argv[3],O_WRONLY);
  ok &= rename("/state/zero","/state/policy-escape")<0;
  ok &= access(argv[4],F_OK)!=0;
  char *args[]={"/system/bin/id",NULL};execv(args[0],args);ok &= errno==EACCES;
  pid_t child=fork(); if(child==0) _exit(1);ok &= child<0 && errno==EACCES;
  ok &= ptrace(PTRACE_TRACEME,0,NULL,NULL)<0 && errno==EACCES;
  printf("uid=%u no_new_privs=1 seccomp=2 filesystem_and_execution_denied=%s\n",getuid(),ok?"true":"false");
  if(argc==10) {
    int network=connects("127.0.0.1",atoi(argv[5]))==1
      && connects("127.0.0.1",atoi(argv[6]))==0 && connects(argv[7],atoi(argv[8]))==0;
    struct addrinfo *answer=NULL;
    int dns_error=getaddrinfo(argv[9],NULL,NULL,&answer);
    int dns=dns_error==0;
    if(!dns)printf("provider_dns_error=%s errno=%d\n",gai_strerror(dns_error),errno);
    if(answer)freeaddrinfo(answer);
    printf("scoped_mcp_reachable=%s lan_and_admin_denied=%s provider_dns=%s\n",network?"true":"false",network?"true":"false",dns?"true":"false");
    ok &= network && dns;
  }
  return ok?0:1;
}
