use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub store: PathBuf,
}

impl Paths {
    pub fn new(home: Option<PathBuf>) -> Result<Self> {
        let home = home
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .or_else(|| std::env::home_dir().map(|h| h.join(".codex")))
            .context("无法确定 Codex 配置目录；请使用 --codex-home")?;
        let home = std::path::absolute(home)?;
        let home = if home.exists() {
            home.canonicalize()?
        } else {
            home
        };
        Ok(Self {
            store: home.join("codex-sw"),
            home,
        })
    }
    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn config_target(&self) -> Result<PathBuf> {
        let path = self.config();
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            path.canonicalize().context("config.toml 是无法解析的软链")
        } else {
            Ok(path)
        }
    }
    pub fn auth(&self) -> PathBuf {
        self.home.join("auth.json")
    }
    pub fn preset(&self, name: &str) -> PathBuf {
        self.store.join("providers").join(format!("{name}.json"))
    }
    pub fn credential(&self, name: &str) -> PathBuf {
        self.store
            .join("credentials")
            .join(format!("auth.json.{name}"))
    }
    pub fn state(&self) -> PathBuf {
        self.store.join("state.json")
    }
    pub fn pending(&self) -> PathBuf {
        self.store.join("pending.json")
    }
    pub fn undo(&self) -> PathBuf {
        self.store.join("undo.json")
    }
    pub fn prepare(&self) -> Result<()> {
        fs::create_dir_all(&self.home).context("无法创建 Codex 配置目录")?;
        for dir in [
            &self.store,
            &self.store.join("providers"),
            &self.store.join("credentials"),
            &self.store.join("history"),
        ] {
            ensure!(
                !fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()),
                "工具存储目录不能是软链"
            );
            fs::create_dir_all(dir).context("无法创建工具存储目录")?;
            private_permissions(dir, true)?;
        }
        Ok(())
    }
    pub fn lock(&self) -> Result<File> {
        self.prepare()?;
        let path = self.store.join("lock");
        ensure!(
            !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
            "工具锁文件不能是软链"
        );
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        private_permissions(&path, false)?;
        file.try_lock()
            .context("另一个 codex-sw 正在修改配置，请稍后重试")?;
        Ok(file)
    }
    pub fn require_clean(&self) -> Result<()> {
        ensure!(
            !self.pending().exists(),
            "检测到未完成事务；请先运行 codex-sw recover"
        );
        Ok(())
    }
    fn validate_target(&self, path: &Path) -> Result<()> {
        ensure!(
            path.is_absolute()
                && !path
                    .components()
                    .any(|c| c == std::path::Component::ParentDir),
            "事务路径无效"
        );
        ensure!(
            path == self.auth() || path == self.config_target()? || path.starts_with(&self.store),
            "事务包含管理范围之外的路径"
        );
        ensure!(!path.is_dir(), "事务目标不能是目录");
        Ok(())
    }
}

pub fn private_permissions(path: &Path, directory: bool) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
        )?;
    }
    #[cfg(windows)]
    {
        windows_private_permissions(path, directory)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_private_permissions(path: &Path, directory: bool) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
            SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    // Use the current user's SID rather than a localized account name.
    unsafe {
        let mut token = ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0,
            "无法读取 Windows 当前用户令牌"
        );
        let mut needed = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        CloseHandle(token);
        ensure!(ok != 0, "无法读取 Windows 当前用户 SID");
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut sid = ptr::null_mut();
        ensure!(
            ConvertSidToStringSidW(user.User.Sid, &mut sid) != 0,
            "无法转换 Windows SID"
        );
        let mut len = 0;
        while *sid.add(len) != 0 {
            len += 1;
        }
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
        LocalFree(sid.cast());
        let inheritance = if directory { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{inheritance};FA;;;{sid_text})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = ptr::null_mut();
        ensure!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut()
            ) != 0,
            "无法创建 Windows 凭据 ACL"
        );
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        ensure!(result != 0, "无法设置 Windows 凭据 ACL");
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Snapshot {
    Missing,
    File {
        data: Vec<u8>,
        mode: Option<u32>,
    },
    Link {
        target: PathBuf,
        data: Option<Vec<u8>>,
    },
}

impl Snapshot {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Missing),
            Err(e) => return Err(e).context("无法读取文件元信息"),
        };
        if metadata.file_type().is_symlink() {
            return Ok(Self::Link {
                target: fs::read_link(path)?,
                data: match fs::read(path) {
                    Ok(data) => Some(data),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e).context("无法读取软链目标"),
                },
            });
        }
        ensure!(metadata.is_file(), "目标必须是普通文件或文件软链");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            Some(metadata.permissions().mode() & 0o777)
        };
        #[cfg(not(unix))]
        let mode = None;
        Ok(Self::File {
            data: fs::read(path)?,
            mode,
        })
    }
    pub fn file(data: impl Into<Vec<u8>>) -> Self {
        Self::File {
            data: data.into(),
            mode: if cfg!(unix) { Some(0o600) } else { None },
        }
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::File { data, .. } => Some(data),
            Self::Link { data, .. } => data.as_deref(),
            Self::Missing => None,
        }
    }
}

pub fn atomic_write(path: &Path, data: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path.parent().context("目标缺少父目录")?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).context("无法创建同目录临时文件")?;
    private_permissions(tmp.path(), false)?;
    tmp.write_all(data)?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .context("无法替换目标文件")?;
    sync_dir(parent)?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn restore(path: &Path, snapshot: &Snapshot) -> Result<()> {
    match snapshot {
        Snapshot::File { data, mode } => atomic_write(path, data, *mode),
        Snapshot::Missing => {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            sync_dir(path.parent().context("目标缺少父目录")?)
        }
        Snapshot::Link { target, .. } => {
            let parent = path.parent().context("目标缺少父目录")?;
            let temp = tempfile::NamedTempFile::new_in(parent)?.into_temp_path();
            fs::remove_file(&temp)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &temp)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(target, &temp)
                .context("无法创建软链；Windows 可使用 --auth-mode copy")?;
            temp.persist(path)
                .map_err(|e| e.error)
                .context("无法替换认证软链")?;
            sync_dir(parent)
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Change {
    pub path: PathBuf,
    pub before: Snapshot,
    pub after: Snapshot,
}

impl Change {
    pub fn new(path: PathBuf, after: Snapshot) -> Result<Self> {
        let before = Snapshot::read(&path)?;
        Ok(Self {
            path,
            before,
            after,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Journal {
    pub format_version: u32,
    pub id: String,
    pub committed: bool,
    pub remember: bool,
    pub changes: Vec<Change>,
}

impl Journal {
    pub fn new(changes: Vec<Change>, remember: bool) -> Self {
        Self {
            format_version: 1,
            id: format!(
                "{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                std::process::id()
            ),
            committed: false,
            remember,
            changes: changes
                .into_iter()
                .filter(|c| c.before != c.after)
                .collect(),
        }
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).context("无法读取工具记录")?;
    serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("工具记录格式无效；未显示可能包含凭据的原文"))
}

fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(data)?, Some(0o600))
}

pub fn execute(paths: &Paths, mut journal: Journal) -> Result<()> {
    paths.require_clean()?;
    if journal.changes.is_empty() {
        return Ok(());
    }
    let mut seen = std::collections::HashSet::new();
    for change in &journal.changes {
        paths.validate_target(&change.path)?;
        ensure!(seen.insert(&change.path), "事务包含重复目标");
        ensure!(
            Snapshot::read(&change.path)? == change.before,
            "文件在准备期间已变化；未执行切换，请重试"
        );
    }
    write_json(&paths.pending(), &journal)?;
    for change in &journal.changes {
        if Snapshot::read(&change.path)? != change.before {
            bail!("检测到并发文件修改；事务已保留，请运行 codex-sw recover");
        }
        restore(&change.path, &change.after)
            .context("写入中断；事务已保留，请运行 codex-sw recover")?;
    }
    journal.committed = true;
    write_json(&paths.pending(), &journal)?;
    finish(paths, &journal)
}

fn finish(paths: &Paths, journal: &Journal) -> Result<()> {
    ensure!(
        !journal.id.is_empty() && journal.id.bytes().all(|c| c.is_ascii_digit() || c == b'-'),
        "事务标识无效"
    );
    write_json(
        &paths
            .store
            .join("history")
            .join(format!("{}.json", journal.id)),
        journal,
    )?;
    if journal.remember {
        write_json(&paths.undo(), journal)?;
    }
    fs::remove_file(paths.pending())?;
    sync_dir(&paths.store)
}

pub fn recover(paths: &Paths) -> Result<bool> {
    if !paths.pending().exists() {
        return Ok(false);
    }
    let journal: Journal = read_json(&paths.pending())?;
    ensure!(journal.format_version == 1, "不支持的事务版本");
    for change in &journal.changes {
        paths.validate_target(&change.path)?;
    }
    if journal.committed {
        finish(paths, &journal)?;
        return Ok(true);
    }
    for change in &journal.changes {
        let current = Snapshot::read(&change.path)?;
        ensure!(
            current == change.before || current == change.after,
            "恢复目标已有外部修改；保留事务及文件，请人工检查"
        );
    }
    for change in journal.changes.iter().rev() {
        let current = Snapshot::read(&change.path)?;
        if current == change.after && current != change.before {
            restore(&change.path, &change.before)?;
        }
    }
    fs::remove_file(paths.pending())?;
    sync_dir(&paths.store)?;
    Ok(true)
}
