use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

#[derive(Clone, Debug, Serialize)]
pub struct Instance {
    pub pid: u32,
    pub scope: String,
}

#[derive(Clone, Default, Serialize)]
pub struct Scan {
    pub instances: Vec<Instance>,
    pub available: bool,
}

pub fn is_codex(name: &OsStr, exe: Option<&Path>, cmd: &[std::ffi::OsString]) -> bool {
    fn native(value: &OsStr) -> bool {
        matches!(
            value.to_string_lossy().to_ascii_lowercase().as_str(),
            "codex" | "codex.exe" | "codex-app-server" | "codex-app-server.exe"
        )
    }
    if native(name) || exe.and_then(Path::file_name).is_some_and(native) {
        return true;
    }
    let node = name.to_string_lossy().to_ascii_lowercase();
    if node == "node" || node == "node.exe" {
        return cmd.iter().take(4).any(|arg| {
            let path = arg.to_string_lossy().replace('\\', "/");
            path.ends_with("/@openai/codex/bin/codex.js")
        });
    }
    false
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

pub fn detect(home: &Path) -> Scan {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .with_environ(UpdateKind::Always),
    );
    let mut scan = Scan {
        available: sysinfo::IS_SUPPORTED_SYSTEM && !system.processes().is_empty(),
        instances: Vec::new(),
    };
    for (pid, process) in system.processes() {
        if pid.as_u32() == std::process::id()
            || !is_codex(process.name(), process.exe(), process.cmd())
        {
            continue;
        }
        let variable = |key: &str| {
            process.environ().iter().find_map(|e| {
                let s = e.to_string_lossy();
                s.strip_prefix(key).map(PathBuf::from)
            })
        };
        let found = variable("CODEX_HOME=").or_else(|| {
            variable("HOME=")
                .or_else(|| variable("USERPROFILE="))
                .map(|h| h.join(".codex"))
        });
        if let Some(found) = &found
            && found.is_absolute()
            && !same_path(found, home)
        {
            continue;
        }
        scan.instances.push(Instance {
            pid: pid.as_u32(),
            scope: if found.is_some_and(|h| h.is_absolute()) {
                "相同 Codex 目录"
            } else {
                "目录无法确认"
            }
            .into(),
        });
    }
    scan.instances.sort_by_key(|p| p.pid);
    scan
}

pub fn warn(scan: &Scan) {
    if !scan.available {
        eprintln!("警告：无法完整检测 Codex 进程；请自行确认活跃实例并重启。");
    }
    if !scan.instances.is_empty() {
        let ids = scan
            .instances
            .iter()
            .map(|p| p.pid.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!("警告：检测到活跃 Codex 实例（PID: {ids}）；已有实例需要重启后使用新配置。");
    }
}
