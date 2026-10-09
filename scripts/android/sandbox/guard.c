// Loaded before ZeroClaw starts any worker threads. No optional fallback.
#include <errno.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sched.h>
#include <stddef.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#define DENY(n) BPF_JUMP(BPF_JMP+BPF_JEQ+BPF_K,(n),0,1), BPF_STMT(BPF_RET+BPF_K,SECCOMP_RET_ERRNO|EACCES)
__attribute__((constructor)) static void protect(void) {
  struct sock_filter f[] = {
    BPF_STMT(BPF_LD+BPF_W+BPF_ABS,offsetof(struct seccomp_data,arch)),
    BPF_JUMP(BPF_JMP+BPF_JEQ+BPF_K,AUDIT_ARCH_AARCH64,1,0),
    BPF_STMT(BPF_RET+BPF_K,SECCOMP_RET_KILL),
    BPF_STMT(BPF_LD+BPF_W+BPF_ABS,offsetof(struct seccomp_data,nr)),
    DENY(__NR_execve), DENY(__NR_execveat), DENY(__NR_ptrace),
    DENY(__NR_mount), DENY(__NR_umount2), DENY(__NR_chroot),
    DENY(__NR_unshare), DENY(__NR_setns), DENY(__NR_bpf),
    DENY(__NR_kexec_load), DENY(__NR_reboot), DENY(__NR_keyctl),
    DENY(__NR_init_module), DENY(__NR_finit_module), DENY(__NR_delete_module),
    DENY(__NR_process_vm_readv), DENY(__NR_process_vm_writev),
    DENY(__NR_open_by_handle_at), DENY(__NR_setuid), DENY(__NR_setgid),
    DENY(__NR_setresuid), DENY(__NR_setresgid), DENY(__NR_setgroups),
#ifdef __NR_clone3
    DENY(__NR_clone3),
#endif
    // Threads remain usable; creating a separate process does not.
    BPF_JUMP(BPF_JMP+BPF_JEQ+BPF_K,__NR_clone,0,3),
    BPF_STMT(BPF_LD+BPF_W+BPF_ABS,offsetof(struct seccomp_data,args[0])),
    BPF_JUMP(BPF_JMP+BPF_JSET+BPF_K,CLONE_THREAD,1,0),
    BPF_STMT(BPF_RET+BPF_K,SECCOMP_RET_ERRNO|EACCES),
    BPF_STMT(BPF_RET+BPF_K,SECCOMP_RET_ALLOW)
  };
  struct sock_fprog p={.len=sizeof(f)/sizeof(f[0]),.filter=f};
  if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0) || prctl(PR_SET_SECCOMP,SECCOMP_MODE_FILTER,&p)) _exit(126);
}
