//! Per-device OpenList setup. Only the selected upstream kind belongs in the
//! portable vault settings. The tools directory and gateway login stay local.
use crate::{openlist_baidu, openlist_runtime};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use zeroize::Zeroizing;

// A sync holds this lease from its initial GET through its conditional PUT.
// Reauthorizing or another local sync must not switch the file mid-round.
static DEPLOYMENT: Mutex<()> = Mutex::new(());
const BUSY: &str = "百度网盘正在同步或部署，请稍后重试";
const SETUP_REQUIRED: &str = "请先在本机完成百度网盘授权和一键部署";

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Setup {
    pub install_parent: Option<PathBuf>,
    pub ready: bool,
}

fn state_path() -> Result<PathBuf, String> {
    Ok(dirs_next::data_local_dir()
        .ok_or("无法定位本机应用数据目录")?
        .join("agent-manager")
        .join("baidu-gateway.json"))
}

pub(crate) fn setup() -> Result<Setup, String> {
    match fs::read(state_path()?) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "百度网盘本机部署记录无法解析".into())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Setup::default()),
        Err(error) => Err(format!("读取百度网盘部署记录失败：{error}")),
    }
}

fn save_setup(state: &Setup) -> Result<(), String> {
    let path = state_path()?;
    let dir = path.parent().ok_or("无法定位本机应用数据目录")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer(tmp.as_file_mut(), state).map_err(|e| e.to_string())?;
    tmp.as_file().sync_all().map_err(|e| e.to_string())?;
    tmp.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn admin_password(create: bool) -> Result<Zeroizing<String>, String> {
    let unavailable = "无法访问本机系统凭据库，请解锁后重试";
    let entry = keyring::Entry::new("com.agentmanager.baidu-openlist.v1", "localhost-admin")
        .map_err(|_| unavailable)?;
    match entry.get_password() {
        Ok(value) => Ok(Zeroizing::new(value)),
        Err(keyring::Error::NoEntry) if create => {
            let value = Zeroizing::new(uuid::Uuid::new_v4().simple().to_string());
            entry.set_password(&value).map_err(|_| unavailable)?;
            Ok(value)
        }
        Err(keyring::Error::NoEntry) => Err(SETUP_REQUIRED.into()),
        Err(_) => Err(unavailable.into()),
    }
}

#[tauri::command]
pub(crate) fn cloud_vault_baidu_setup() -> Result<Setup, String> {
    setup()
}

#[tauri::command]
pub(crate) async fn cloud_vault_baidu_deploy(
    install_parent: String,
    refresh_token: String,
    progress: tauri::ipc::Channel<String>,
) -> Result<Setup, String> {
    let refresh_token = Zeroizing::new(refresh_token);
    tauri::async_runtime::spawn_blocking(move || {
        let _exclusive = DEPLOYMENT.try_lock().map_err(|_| BUSY)?;
        let parent = Path::new(&install_parent);
        if !parent.is_absolute() || !parent.is_dir() {
            return Err("请选择已存在的本机工具安装目录".into());
        }
        let token = refresh_token.trim();
        if token.is_empty() || token.len() > 8192 || token.contains(char::is_whitespace) {
            return Err("请粘贴百度授权返回的 Refresh Token".into());
        }
        let password = admin_password(true)?;
        let _ = progress.send("installing".into());
        openlist_runtime::install(parent)?;
        let _ = progress.send("starting".into());
        let port = openlist_runtime::start(parent, &password)?;
        // The driver may already have switched accounts when provisioning fails.
        // Keep that instance out of scheduled sync until remote access is verified.
        let mut state = Setup {
            install_parent: Some(parent.to_owned()),
            ready: false,
        };
        save_setup(&state)?;
        let _ = progress.send("mounting".into());
        openlist_baidu::provision(port, &password, token)?;
        state.ready = true;
        save_setup(&state)?;
        Ok(state)
    })
    .await
    .map_err(|_| "百度网盘部署任务中断")?
}

pub(crate) struct Session {
    pub url: String,
    pub password: Zeroizing<String>,
    pub lease: MutexGuard<'static, ()>,
}

pub(crate) fn connect() -> Result<Session, String> {
    let lease = DEPLOYMENT.try_lock().map_err(|_| BUSY)?;
    let state = setup()?;
    let parent = state
        .install_parent
        .filter(|_| state.ready)
        .ok_or(SETUP_REQUIRED)?;
    let password = admin_password(false)?;
    let port = openlist_runtime::start(&parent, &password)?;
    Ok(Session {
        url: format!("http://127.0.0.1:{port}{}", openlist_baidu::DAV_PATH),
        password,
        lease,
    })
}
