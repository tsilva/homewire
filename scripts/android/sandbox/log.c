// Bounded supervisor logging. Input is bytes; it never becomes commands/paths.
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>
int main(int argc,char **argv) {
  if(argc!=2)return 1;
  char current[PATH_MAX],previous[PATH_MAX],buf[1024];
  if(snprintf(current,sizeof(current),"%s/current",argv[1])>=PATH_MAX ||
     snprintf(previous,sizeof(previous),"%s/previous",argv[1])>=PATH_MAX)return 1;
  int fd=-1;ssize_t n;
  while((n=read(0,buf,sizeof(buf)))>0) {
    if(fd<0) {fd=open(current,O_WRONLY|O_CREAT|O_APPEND|O_NOFOLLOW,0600);if(fd<0)return 1;}
    struct stat s;if(fstat(fd,&s))return 1;
    if(s.st_size+n>65536) {
      close(fd);if(rename(current,previous))return 1;
      fd=open(current,O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW,0600);if(fd<0)return 1;
    }
    for(ssize_t off=0;off<n;) {ssize_t written=write(fd,buf+off,n-off);if(written<=0)return 1;off+=written;}
  }
  if(fd>=0)close(fd);return n<0?1:0;
}
