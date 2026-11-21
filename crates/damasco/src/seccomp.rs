//! The seccomp filter of the sandbox: system calls that untrusted code never
//! needs, and that widen the attack surface of the kernel, fail with EPERM.
//! Everything else is allowed. bubblewrap loads the filter after setting up
//! the namespaces, just before running the command.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// libc defines it for x86_64 but not for aarch64 with musl, where the
/// number comes from the kernel's generic table.
#[cfg(target_arch = "x86_64")]
const SYS_KEXEC_FILE_LOAD: i64 = libc::SYS_kexec_file_load;
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: i64 = 294;

/// System calls refused outright.
fn refused() -> Vec<i64> {
    let mut calls = vec![
        // Kernel modules, kexec, reboot, swap, accounting, quotas.
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_kexec_load,
        SYS_KEXEC_FILE_LOAD,
        libc::SYS_reboot,
        libc::SYS_swapon,
        libc::SYS_swapoff,
        libc::SYS_acct,
        libc::SYS_quotactl,
        // Kernel log, keyring, eBPF, performance counters.
        libc::SYS_syslog,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        // Frequent sources of kernel exploits.
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        // Mounts and namespaces: the layout set by bubblewrap is final.
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_fspick,
        libc::SYS_move_mount,
        libc::SYS_open_tree,
        libc::SYS_mount_setattr,
        libc::SYS_setns,
        libc::SYS_unshare,
        // File handles that bypass the mount namespace.
        libc::SYS_open_by_handle_at,
        libc::SYS_name_to_handle_at,
        libc::SYS_vhangup,
    ];
    #[cfg(target_arch = "x86_64")]
    calls.extend([
        libc::SYS_iopl,
        libc::SYS_ioperm,
        libc::SYS_modify_ldt,
        libc::SYS_uselib,
    ]);
    calls
}

/// ioctl requests refused: pushing input into the terminal, and the Linux
/// console's own commands.
// The type of the ioctl constants differs between C libraries; the casts
// keep the list portable.
#[allow(clippy::unnecessary_cast)]
const REFUSED_IOCTLS: [u64; 2] = [libc::TIOCSTI as u64, libc::TIOCLINUX as u64];

fn arch() -> Result<TargetArch, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok(TargetArch::x86_64),
        "aarch64" => Ok(TargetArch::aarch64),
        other => Err(format!("no seccomp filter for {other}")),
    }
}

fn compile() -> Result<Vec<u8>, String> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = refused()
        .into_iter()
        .map(|call| (call, Vec::new()))
        .collect();
    let ioctl = REFUSED_IOCTLS
        .iter()
        .map(|request| {
            let condition =
                SeccompCondition::new(1, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, *request)
                    .map_err(|e| e.to_string())?;
            SeccompRule::new(vec![condition]).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    rules.insert(libc::SYS_ioctl, ioctl);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        arch()?,
    )
    .map_err(|e| e.to_string())?;
    let program: BpfProgram = filter
        .try_into()
        .map_err(|e: seccompiler::BackendError| e.to_string())?;
    // bubblewrap reads struct sock_filter instructions in native byte order.
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for instruction in program {
        bytes.extend_from_slice(&instruction.code.to_ne_bytes());
        bytes.push(instruction.jt);
        bytes.push(instruction.jf);
        bytes.extend_from_slice(&instruction.k.to_ne_bytes());
    }
    Ok(bytes)
}

/// The compiled filter, built once.
pub fn program() -> Result<&'static [u8], String> {
    static PROGRAM: OnceLock<Result<Vec<u8>, String>> = OnceLock::new();
    PROGRAM
        .get_or_init(compile)
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_to_whole_instructions() {
        let bytes = program().unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(bytes.len() % 8, 0);
    }
}
