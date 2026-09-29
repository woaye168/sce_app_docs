//! 构建管线：vitepress build 短命 spawn（起→跑→自己退出）。
//! Job Object（KILL_ON_JOB_CLOSE）双保险：app 异常退出时 OS 自动杀掉构建进程，
//! 不依赖句柄/PID 文件/taskkill——孤儿这个类别在机制上不存在。
//!
//! 原子切换：每次构建到轮换目录（dist-a / dist-b），成功后由调用方翻转服务根再删旧目录，
//! 构建期间旧 dist 照常服务不掉线。

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 判断已装 vitepress 是否为 2.x（读 node_modules/vitepress/package.json 版本号）
fn is_vitepress_v2(home: &Path) -> bool {
    let pkg = home.join("node_modules/vitepress/package.json");
    let Ok(content) = std::fs::read_to_string(&pkg) else { return false };
    content.contains("\"version\": \"2.")
}

/// 确保全局依赖（VitePress 2.x alpha，内置 minisearch 本地搜索）。
/// package.json 每次重写（依赖清单变化要生效），node_modules 缺包才跑 install。
pub fn ensure_deps(home: &Path, node: &Path) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("创建 vitepress_home 失败: {e}"))?;
    let pkg = home.join("package.json");
    let _ = std::fs::write(&pkg, r#"{"name":"bgd-docs-vitepress","private":true,"type":"module","devDependencies":{"vitepress":"^2.0.0-alpha.20"}}"#);
    let need_install = !home.join("node_modules/vitepress").is_dir() || !is_vitepress_v2(home);
    if !need_install {
        return Ok(());
    }
    let npm = node.with_file_name("npm.cmd");
    let mut cmd = Command::new(npm);
    cmd.arg("install").current_dir(home);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!("VitePress 依赖安装失败: {}", String::from_utf8_lossy(&o.stderr))),
        Err(e) => Err(format!("npm install 启动失败: {e}")),
    }
}

/// Windows Job Object：关闭句柄即杀光组内进程（OS 级保证，穿壳防孤儿）
#[cfg(windows)]
struct JobGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// 创建 KILL_ON_JOB_CLOSE Job 并把子进程装进去（失败返回 None，降级裸跑——构建进程本来也短命）
#[cfg(windows)]
fn assign_to_job(child_id: u32) -> Option<JobGuard> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::OpenProcess;
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0 {
            CloseHandle(job);
            return None;
        }
        // AssignProcessToJobObject 需要 SET_QUOTA | TERMINATE 权限
        let proc = OpenProcess(0x0100 | 0x0001, 0, child_id); // PROCESS_SET_QUOTA | PROCESS_TERMINATE
        if proc.is_null() {
            CloseHandle(job);
            return None;
        }
        let ok = AssignProcessToJobObject(job, proc);
        CloseHandle(proc);
        if ok == 0 {
            CloseHandle(job);
            return None;
        }
        Some(JobGuard(job))
    }
}

/// 构建站点：node vitepress.js build . --outDir <out_name>（cwd = site）。
/// out_name 用 dist-a / dist-b 轮换：构建新目录时不碰正在服务的旧目录（原子切换前提）。
/// 成功返回新 dist 目录路径；失败返回日志尾部错误。构建输出写 site/build.log。
pub fn build(home: &Path, node: &Path, site: &Path, out_name: &str) -> Result<PathBuf, String> {
    let vp = home.join("node_modules/vitepress/bin/vitepress.js");
    if !vp.is_file() {
        return Err(format!("vitepress 未安装（缺 {}）", vp.display()));
    }
    let out_dir = site.join(out_name);
    let _ = std::fs::remove_dir_all(&out_dir); // 清上次残留
    let log_path = site.join("build.log");
    let log_file = std::fs::File::create(&log_path).ok();

    let mut cmd = Command::new(node);
    cmd.arg(&vp).arg("build").arg(".").arg("--outDir").arg(out_name);
    cmd.current_dir(site);
    if let Some(f) = log_file {
        match f.try_clone() {
            Ok(f2) => {
                cmd.stdout(f);
                cmd.stderr(f2);
            }
            Err(_) => {
                cmd.stdout(std::process::Stdio::null());
                cmd.stderr(std::process::Stdio::null());
            }
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|e| format!("vitepress build 启动失败: {e}"))?;
    #[cfg(windows)]
    let _job = assign_to_job(child.id()); // 句柄活到本函数结束（child.wait 之后）
    let status = child.wait().map_err(|e| format!("等待构建进程失败: {e}"))?;
    if status.success() && out_dir.join("index.html").is_file() {
        Ok(out_dir)
    } else {
        Err(format!("vitepress build 失败（exit={status:?}）：{}", read_log_tail(&log_path)))
    }
}

/// 读构建日志尾部（错误定位用）
fn read_log_tail(path: &Path) -> String {
    std::fs::read_to_string(path)
        .map(|c| {
            c.lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .unwrap_or_else(|_| "（无日志）".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fails_without_vitepress() {
        let tmp = std::env::temp_dir().join(format!("bgd_docs_build_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let fake_node = PathBuf::from("node");
        let r = build(&tmp, &fake_node, &tmp, "dist-a");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("vitepress 未安装"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
