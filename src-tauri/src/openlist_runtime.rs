//! The installation is the pinned archive. Each cold start extracts a fresh private
//! executable from a verified snapshot. Only the archive and OpenList data need to persist.

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::{blocking::Client, redirect::Policy};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};
use uuid::Uuid;

type RuntimeResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
const VERSION: &str = "v4.2.9";
const DIRECTORY: &str = "openlist-managed";
const EXECUTABLE: &str = if cfg!(windows) { "openlist.exe" } else { "openlist" };
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const READY_TIMEOUT: Duration = Duration::from_secs(35);
const TICK: Duration = Duration::from_millis(100);

struct Asset {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

fn asset() -> RuntimeResult<Asset> {
    // assets[].{name,size,digest} from api.github.com/repos/OpenListTeam/OpenList/releases/tags/v4.2.9.
    let (name, size, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => (
            "openlist-windows-amd64.zip",
            53841156,
            "144495827fba561f44b85b83c4135d676236c81785e104f4e9e8bbe9d540b4c8",
        ),
        ("windows", "aarch64") => (
            "openlist-windows-arm64.zip",
            50387597,
            "863761c9e66a882f5a8e2c26e9d5a1ae0537747d01442f146f35747d5cbe24f3",
        ),
        ("macos", "x86_64") => (
            "openlist-darwin-amd64.tar.gz",
            53487672,
            "2822e7d63855deacc7b816171ca14aaaeae59fec97c955d7d4258eedbfe4b881",
        ),
        ("macos", "aarch64") => (
            "openlist-darwin-arm64.tar.gz",
            51578042,
            "dac3b46e96f18ec3a06824fbac660d3e23c8a96f0aa40bc2bf2c5fde5b1eaecf",
        ),
        ("linux", "x86_64") => (
            "openlist-linux-amd64.tar.gz",
            53440472,
            "1f41b64c61f66bc632490a0e17ee8769454fe093f03a68d22ffb4aa8a9b23187",
        ),
        ("linux", "aarch64") => (
            "openlist-linux-arm64.tar.gz",
            50349406,
            "b8dd2a8db97c4a57d97cd0dcf725826be228cf72ac5b2ff235d0ba5c92ac4a4c",
        ),
        _ => return Err("OpenList v4.2.9 requires Windows, macOS or Linux on x64 or ARM64".into()),
    };
    Ok(Asset { name, size, sha256 })
}

struct Running {
    // Drop the process before removing its image or releasing the installation lock.
    process: OwnedChild,
    _image: TempDir,
    _lock: File,
    root: PathBuf,
    port: u16,
    marker: String,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn state() -> MutexGuard<'static, Option<Running>> {
    RUNNING.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub(super) fn install(parent: &Path) -> Result<(), String> {
    install_inner(parent).map_err(|e| format!("OpenList installation: {e}"))
}

pub(super) fn start(parent: &Path, admin_password: &str) -> Result<u16, String> {
    start_inner(parent, admin_password).map_err(|e| format!("OpenList startup: {e}"))
}

pub(super) fn stop() {
    let mut slot = state();
    drop(slot.take());
}

fn managed_root(parent: &Path, create: bool) -> RuntimeResult<PathBuf> {
    if !parent.is_absolute() {
        return Err("select an absolute local tools directory".into());
    }
    // The selected directory may itself be a link. Resolve it once, then reject
    // redirections inside the dedicated area (including Windows junctions).
    let parent = fs::canonicalize(parent)?;
    #[cfg(windows)]
    let parent = {
        let path = parent.to_str().ok_or("invalid tools directory")?;
        let path = path.strip_prefix(r"\\?\").unwrap_or(path);
        if !matches!(Path::new(path).components().next(), Some(std::path::Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_)))
        {
            return Err("select a local drive for OpenList".into());
        }
        PathBuf::from(path)
    };
    if parent.to_string_lossy().contains('?') {
        return Err("OpenList's SQLite driver cannot use a directory containing '?'".into());
    }
    let root = parent.join(DIRECTORY);
    let created = checked_directory(&root, create)?;
    let marker = root.join(".vibe-assistant-owner");
    let expected = format!("Vibe Assistant managed OpenList\nlayout=1\nrelease={VERSION}\n");
    if !created {
        let mut bytes = Vec::new();
        if regular_file(&marker)? {
            File::open(&marker)?
                .take(expected.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
        }
        if bytes != expected.as_bytes() {
            return Err(
                "existing openlist-managed directory has no matching Vibe Assistant ownership marker".into(),
            );
        }
    }
    protect_directory(&root)?;
    if created {
        let mut file = File::create_new(&marker)?;
        file.write_all(expected.as_bytes())?;
        file.sync_all()?;
    }
    Ok(root)
}

fn redirected(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn checked_directory(path: &Path, create: bool) -> RuntimeResult<bool> {
    let created = if create {
        #[cfg(unix)]
        let result = {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(path)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(path);
        match result {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        }
    } else {
        false
    };
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || redirected(&meta) {
        return Err("expected a real directory in the managed OpenList installation".into());
    }
    Ok(created)
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(windows)]
fn protect_directory(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
        SetFileSecurityW, SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TokenUser, ACL_REVISION,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_DESCRIPTOR,
        SE_DACL_PROTECTED, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // An inheritable DACL granting only this user access, including when a parent
    // tools directory is shared. OpenList stores provider tokens in SQLite/WAL.
    let name: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut token = std::ptr::null_mut();
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::from_raw_handle(token);
        // TOKEN_USER plus a maximum 68-byte SID; ACL header plus one ACE and SID.
        // Word arrays provide the alignment required by these variable-size structs.
        let mut user = [0usize; 32];
        let mut acl = [0u32; 32];
        let mut written = 0;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let sd = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
        let entries = acl.as_mut_ptr().cast();
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            user.as_mut_ptr().cast(),
            std::mem::size_of_val(&user) as u32,
            &mut written,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        const FILE_ALL_ACCESS: u32 = 0x001f_01ff;
        if InitializeAcl(entries, std::mem::size_of_val(&acl) as u32, ACL_REVISION) == 0
            || AddAccessAllowedAceEx(
                entries,
                ACL_REVISION,
                SUB_CONTAINERS_AND_OBJECTS_INHERIT,
                FILE_ALL_ACCESS,
                sid,
            ) == 0
            || InitializeSecurityDescriptor(sd, 1) == 0
            || SetSecurityDescriptorDacl(sd, 1, entries, 0) == 0
            || SetSecurityDescriptorControl(sd, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
            || SetFileSecurityW(
                name.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                sd,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn regular_file(path: &Path) -> RuntimeResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !redirected(&meta) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err("unexpected hard link in the managed installation".into());
                }
            }
            Ok(true)
        }
        Ok(_) => Err("unexpected file type in the managed OpenList installation".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn lock_root(root: &Path) -> RuntimeResult<File> {
    let path = root.join("runtime.lock");
    regular_file(&path)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.try_lock()
        .map_err(|_| "the OpenList directory is in use or does not support file locking")?;
    Ok(file)
}

impl Asset {
    fn path(&self, root: &Path) -> PathBuf {
        root.join(format!("{VERSION}-{}", self.name))
    }

    fn copy_verified(&self, mut input: impl Read, mut output: impl Write) -> RuntimeResult<()> {
        let mut buffer = [0; 64 * 1024];
        let mut length = 0u64;
        let mut hash = Sha256::new();
        let deadline = Instant::now() + DOWNLOAD_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err("archive read timed out".into());
            }
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            length += n as u64;
            if length > self.size {
                return Err("archive exceeds the pinned length".into());
            }
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
        }
        if length != self.size || format!("{:x}", hash.finalize()) != self.sha256 {
            return Err("OpenList release archive failed SHA-256 or length verification".into());
        }
        Ok(())
    }
}

fn install_inner(parent: &Path) -> RuntimeResult<()> {
    let slot = state();
    let asset = asset()?;
    let root = managed_root(parent, true)?;
    let _lock = if slot.as_ref().is_some_and(|r| r.root == root) {
        None
    } else {
        Some(lock_root(&root)?)
    };
    let path = asset.path(&root);
    if regular_file(&path)? && asset.copy_verified(File::open(&path)?, io::sink()).is_ok() {
        return Ok(());
    }
    let redirects = Policy::custom(|attempt| {
        let url = attempt.url();
        if attempt.previous().len() < 5
            && url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && matches!(
                url.host_str(),
                Some("github.com" | "release-assets.githubusercontent.com" | "objects.githubusercontent.com")
            )
        {
            attempt.follow()
        } else {
            attempt.error("unexpected GitHub release redirect")
        }
    });
    let client = Client::builder()
        .https_only(true)
        .redirect(redirects)
        .referer(false)
        .connect_timeout(Duration::from_secs(10))
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?;
    let url = format!(
        "https://github.com/OpenListTeam/OpenList/releases/download/{VERSION}/{}",
        asset.name
    );
    let response = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(reqwest::Error::without_url)?;
    let mut staged = NamedTempFile::new_in(&root)?;
    asset.copy_verified(response, staged.as_file_mut())?;
    staged.as_file().sync_all()?;
    staged.persist(path)?;
    Ok(())
}

fn prepare_image(asset: &Asset, root: &Path) -> RuntimeResult<TempDir> {
    let path = asset.path(root);
    if !regular_file(&path)? {
        return Err("install the pinned OpenList release first".into());
    }
    let image = tempfile::Builder::new().prefix("run-").tempdir_in(root)?;
    let mut snapshot = tempfile::tempfile_in(image.path())?;
    asset.copy_verified(File::open(path)?, &mut snapshot)?;
    snapshot.rewind()?;
    let out = File::create_new(image.path().join(EXECUTABLE))?;
    #[cfg(windows)]
    {
        let mut zip = zip::ZipArchive::new(snapshot)?;
        if zip.len() != 1 {
            return Err("unexpected OpenList ZIP layout".into());
        }
        let mut member = zip.by_index(0)?;
        if member.name() != EXECUTABLE || !member.is_file() || member.size() == 0 || member.size() > 512 << 20
        {
            return Err("unexpected OpenList ZIP member".into());
        }
        let mut out = out;
        let expected = member.size();
        if io::copy(&mut member, &mut out)? != expected {
            return Err("incomplete OpenList executable".into());
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // v4.2.9 build.sh archives the single regular member "openlist". Output its
        // bytes to our own file descriptor. tar never extracts a pathname to disk.
        let tar = if Path::new("/usr/bin/tar").is_file() {
            "/usr/bin/tar"
        } else {
            "/bin/tar"
        };
        let status = Command::new(tar)
            .args(["-xzOf", "-", "openlist"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir("/")
            .stdin(snapshot)
            .stdout(out.try_clone()?)
            .stderr(Stdio::null())
            .status()?;
        if !status.success() || !(1..=512 << 20).contains(&out.metadata()?.len()) {
            return Err("could not extract the pinned OpenList executable with system tar".into());
        }
        out.set_permissions(fs::Permissions::from_mode(0o700))?;
    }
    Ok(image)
}

fn write_config(image: &Path, port: i32) -> RuntimeResult<()> {
    let origin = format!("http://127.0.0.1:{}", port.max(1));
    // v4.2.9 overlays this on defaults. force suppresses OPENLIST_* overrides.
    // An absolute site_url also prevents Host/X-Forwarded-Host rewriting generated links.
    let config = json!({
        "force": true, "site_url": origin, "last_launched_version": VERSION,
        "scheme": { "address": "127.0.0.1", "http_port": port, "https_port": -1,
            "unix_file": "", "enable_h2c": false, "enable_h3": false, "force_https": false },
        "s3": { "enable": false, "port": -1 }, "ftp": { "enable": false, "listen": "" },
        "sftp": { "enable": false, "listen": "" }, "mcp": { "enable": false },
        "cors": { "allow_origins": [origin] }, "log": { "enable": false }
    });
    fs::write(image.join("config.json"), serde_json::to_vec(&config)?)?;
    Ok(())
}

fn password_hash(password: &str, salt: &str) -> String {
    // internal/model/user.go: hex(SHA256(hex(SHA256(password + static suffix)) + "-" + salt)).
    let static_hash = Sha256::new()
        .chain_update(password)
        .chain_update(b"-https://github.com/alist-org/alist")
        .finalize();
    format!("{:x}", Sha256::digest(format!("{static_hash:x}-{salt}")))
}

fn secure_accounts(data: &Path, password: &str, marker: &str) -> RuntimeResult<()> {
    let mut db = Connection::open_with_flags(data.join("data.db"), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    db.busy_timeout(Duration::from_secs(2))?;
    let tx = db.transaction()?;
    let salt = Uuid::new_v4().simple().to_string();
    // OPENLIST_ADMIN_PASSWORD initializes the account only once. Reconcile the
    // supplied password offline, before there is a listener or a populated user cache.
    // The upstream admin default has WebDAV read (bit 8), but uploads and deletes
    // also require WebDAV manage (bit 9), even for role ADMIN.
    let admin = tx.execute(
        "UPDATE x_users SET username='admin', disabled=0, permission=permission | 768,
        password='', pwd_hash=?1, salt=?2, pwd_ts=unixepoch() WHERE role=2 AND coalesce(otp_secret,'')=''",
        params![password_hash(password, &salt), salt],
    )?;
    let guest = tx.execute(
        "UPDATE x_users SET disabled=1, permission=0, password='', pwd_hash='!', allow_ldap=0 WHERE role=1",
        [],
    )?;
    if admin != 1 || guest != 1 {
        return Err("expected one password-managed admin and one guest account".into());
    }
    // Public shares bypass the disabled guest. Sharing.Valid checks the creator's
    // CanShare bit even for existing shares. Signed downloads remain credentialed links.
    tx.execute(
        "UPDATE x_users SET permission=permission & ~49152 WHERE (permission & 49152) != 0",
        [],
    )?;
    for (key, value) in [
        ("sign_all", "true"),
        ("link_expiration", "1"),
        ("robots_txt", marker),
        ("ldap_login_enabled", "false"),
        ("sso_login_enabled", "false"),
        ("media_enabled", "false"),
        ("transcode_enabled", "false"),
    ] {
        if tx.execute(
            "UPDATE x_setting_items SET value=?1 WHERE key=?2",
            params![value, key],
        )? != 1
        {
            return Err("missing security setting in the OpenList database".into());
        }
    }
    // Auth compares the Authorization header to this setting before checking the guest.
    let token = Uuid::new_v4().simple().to_string();
    if tx.execute("UPDATE x_setting_items SET value=CASE WHEN length(trim(coalesce(value,''))) < 32 THEN ?1 ELSE value END WHERE key='token'", [token])? != 1 {
        return Err("missing OpenList API token setting".into());
    }
    tx.commit()?;
    Ok(())
}

struct OwnedChild {
    child: Child,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

impl OwnedChild {
    fn spawn(image: &Path, subcommand: &str, initial_password: Option<&str>) -> RuntimeResult<Self> {
        let executable = image.join(EXECUTABLE);
        #[cfg(not(unix))]
        let mut cmd = Command::new(executable);
        #[cfg(unix)]
        let mut cmd = {
            use std::os::unix::process::CommandExt;
            // The pipe closes on parent death, including a crash. Its reader remains
            // in this private process group, so SIGTERM to group 0 cannot target a recycled PID.
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "umask 077; exec 3<&0; (IFS= read -r x <&3; kill -TERM 0) & exec 3<&-; exec \"$@\" </dev/null", "openlist-guardian"])
                .arg(executable).process_group(0).stdin(Stdio::piped());
            cmd
        };
        cmd.env_clear()
            .current_dir(image)
            .args([subcommand, "--data", "../data", "--config", "config.json"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for key in [
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "TMPDIR",
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "LANG",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        #[cfg(unix)]
        cmd.env("PATH", "/usr/bin:/bin");
        #[cfg(windows)]
        {
            cmd.stdin(Stdio::null());
            crate::process_util::no_window(&mut cmd);
        }
        if let Some(password) = initial_password {
            cmd.env("OPENLIST_ADMIN_PASSWORD", password);
        }
        let child = cmd.spawn()?;
        #[cfg(windows)]
        {
            let mut child = child;
            match child_job(&child) {
                Ok(job) => Ok(Self { child, _job: job }),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err(e.into())
                }
            }
        }
        #[cfg(not(windows))]
        {
            Ok(Self { child })
        }
    }

    fn wait_until(&mut self, deadline: Instant) -> RuntimeResult<ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err("OpenList process timed out".into());
            }
            thread::sleep(TICK);
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            drop(self.child.stdin.take());
            if self.wait_until(Instant::now() + Duration::from_secs(2)).is_ok() {
                return;
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
fn child_job(child: &Child) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

#[derive(PartialEq)]
enum Health {
    Ready,
    Pending,
    WrongListener,
}

fn probe(client: &Client, port: u16, marker: &str) -> Health {
    // Neither request sends a credential or the expected instance marker. /ping
    // alone cannot distinguish the child from another process that won the bind.
    let get = |path| -> Result<Vec<u8>, Health> {
        let response = client
            .get(format!("http://127.0.0.1:{port}/{path}"))
            .timeout(Duration::from_millis(900))
            .send()
            // A cold Windows listener can time out before accepting connections.
            // Only an HTTP response can establish a different listener's identity.
            .map_err(|_| Health::Pending)?;
        if !response.status().is_success() {
            #[cfg(test)]
            eprintln!("OpenList readiness {path}: HTTP {}", response.status().as_u16());
            return Err(Health::WrongListener);
        }
        let mut body = Vec::new();
        response
            .take(4097)
            .read_to_end(&mut body)
            .map_err(|_| Health::Pending)?;
        Ok(body)
    };
    match get("robots.txt") {
        Err(state) => return state,
        Ok(body) if body != marker.as_bytes() => {
            #[cfg(test)]
            eprintln!("OpenList readiness: different marker, {} bytes, whitespace-only difference: {}",
                body.len(), std::str::from_utf8(&body).is_ok_and(|s| s.trim() == marker.trim()));
            return Health::WrongListener;
        },
        _ => (),
    }
    let denied = get("api/me")
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    if denied.as_ref().and_then(|v| v["code"].as_i64()) == Some(401) {
        Health::Ready
    } else {
        Health::Pending
    }
}

fn admin_matches(root: &Path, password: &str) -> RuntimeResult<bool> {
    let db = Connection::open_with_flags(root.join("data/data.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(500))?;
    // Detect a password change or an unsafe persisted policy without ever sending
    // the caller's password (or its login-equivalent static hash) to the probe port.
    let (salt, hash): (String, String) = db.query_row(
        "SELECT salt, pwd_hash FROM x_users WHERE role=2
        AND username='admin' AND disabled=0 AND coalesce(otp_secret,'')='' AND (permission & 768)=768
        AND NOT EXISTS (SELECT 1 FROM x_users WHERE (permission & 49152) != 0)
        AND EXISTS (SELECT 1 FROM x_setting_items WHERE key='sign_all' AND value='true')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(password_hash(password, &salt) == hash)
}

fn start_inner(parent: &Path, password: &str) -> RuntimeResult<u16> {
    if password.is_empty() || password.contains('\0') {
        return Err("invalid managed admin password".into());
    }
    let mut slot = state();
    let asset = asset()?;
    let root = managed_root(parent, false)?;
    let client = Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .timeout(Duration::from_millis(900))
        .build()?;
    if let Some(running) = slot.as_mut().filter(|r| r.root == root) {
        if running.process.child.try_wait()?.is_none()
            && admin_matches(&root, password).unwrap_or(false)
            && probe(&client, running.port, &running.marker) == Health::Ready
            && running.process.child.try_wait()?.is_none()
        {
            return Ok(running.port);
        }
        drop(slot.take());
    }
    // When changing directories, keep the old instance until its replacement is ready.
    let lock = lock_root(&root)?;
    let image = prepare_image(&asset, &root)?;
    let data = root.join("data");
    for dir in [&data, &data.join("temp")] {
        checked_directory(dir, true)?;
        protect_directory(dir)?;
    }
    for name in ["data.db", "data.db-wal", "data.db-shm", "data.db-journal"] {
        regular_file(&data.join(name))?;
    }
    let marker = format!("User-agent: *\nDisallow: /\n# {}\n", Uuid::new_v4());
    write_config(image.path(), -1)?;
    {
        // This upstream subcommand initializes schema/users and never starts listeners.
        // v4.2.9 prints the initial password to stdout. Both streams go to the null device.
        let mut setup = OwnedChild::spawn(image.path(), "admin", Some(password))?;
        if !setup.wait_until(Instant::now() + READY_TIMEOUT)?.success() {
            return Err("OpenList could not initialize its private data directory".into());
        }
    }
    secure_accounts(&data, password, &marker)?;
    let deadline = Instant::now() + READY_TIMEOUT;
    for _ in 0..3 {
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = reservation.local_addr()?.port();
        write_config(image.path(), i32::from(port))?;
        drop(reservation);
        let mut process = OwnedChild::spawn(image.path(), "server", None)?;
        loop {
            if process.child.try_wait()?.is_some() {
                return Err("OpenList exited during startup".into());
            }
            if Instant::now() >= deadline {
                return Err("OpenList loopback readiness timed out".into());
            }
            match probe(&client, port, &marker) {
                Health::Ready if process.child.try_wait()?.is_none() => {
                    *slot = Some(Running {
                        process,
                        _image: image,
                        _lock: lock,
                        root,
                        port,
                        marker,
                    });
                    return Ok(port);
                }
                Health::WrongListener => break,
                _ => thread::sleep(TICK),
            }
        }
        // Dropping the owned child releases its resources before another bind attempt.
    }
    Err("OpenList could not acquire a private loopback port".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_retries_a_dropped_connection_but_rejects_a_different_marker() {
        let client = Client::builder().no_proxy().redirect(Policy::none()).build().unwrap();
        for (reply, expected) in [(None, Health::Pending), (Some("other"), Health::WrongListener)] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request).unwrap();
                if let Some(body) = reply {
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            assert!(probe(&client, port, "expected marker") == expected);
            worker.join().unwrap();
        }
    }
}
