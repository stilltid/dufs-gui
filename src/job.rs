//! Windows Job Object 封装 —— 保证 dufs 子进程不会变成孤儿。
//!
//! 为什么必须做这件事：
//!   只靠 `Drop` 去 kill 子进程是不够的。GUI 进程一旦被任务管理器"结束任务"、
//!   或者 panic 触发 abort，`Drop` 根本不会执行，dufs 就会一直留在后台占着端口，
//!   用户下次启动会看到"端口被占用"，而且任务管理器里有个看不见的 dufs 进程。
//!
//! Job Object 的 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 由内核保证：
//!   只要本进程消失（无论正常退出、崩溃、还是被强杀），它持有的 job 句柄被关闭，
//!   系统就会连带杀死 job 里的所有进程。

#![cfg(windows)]

use std::io;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
    JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Job 句柄。Drop 时会关闭句柄 —— 而关闭句柄这个动作本身就会连带杀死子进程。
pub struct JobHandle(HANDLE);

// HANDLE 是裸指针，默认不是 Send/Sync，但它本质上只是一个内核句柄（整数），
// 跨线程传递是安全的。
unsafe impl Send for JobHandle {}
unsafe impl Sync for JobHandle {}

impl JobHandle {
    /// 创建一个"关闭即杀子进程"的 Job
    pub fn create_kill_on_close() -> io::Result<Self> {
        unsafe {
            let job = CreateJobObjectW(None, None).map_err(to_io)?;

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

            let r = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if let Err(e) = r {
                let _ = CloseHandle(job);
                return Err(to_io(e));
            }

            Ok(JobHandle(job))
        }
    }

    /// 把一个已启动的子进程加入本 Job
    pub fn assign(&self, pid: u32) -> io::Result<()> {
        unsafe {
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)
                .map_err(to_io)?;
            let r = AssignProcessToJobObject(self.0, proc);
            // 无论成功与否都要关掉这个进程句柄；job 已经持有引用，子进程不会因此退出
            let _ = CloseHandle(proc);
            r.map_err(to_io)
        }
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn to_io(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}
