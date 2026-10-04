use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Child;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: String,
    pub env: HashMap<String, String>,
    pub port: Option<u16>,
    #[serde(default)]
    pub ui_token: Option<String>,
    /// 可选 Git worktree 隔离：源仓库绝对路径（须为 git 仓库）。设置后启动/
    /// 打开终端时惰性创建独立工作树作为 cwd；未设置沿用 working_dir。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_repo: Option<String>,
    /// worktree 检出分支：已存在则检出，不存在则自 HEAD 创建；缺省分离 HEAD。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_branch: Option<String>,
    pub auto_restart: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Stopped,
    Error,
    Starting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentState {
    pub config: AgentConfig,
    pub status: AgentStatus,
    pub pid: Option<u32>,
    pub started_at: Option<String>,
    pub port_open: bool,
    /// 自动重启累计次数（手动 start_agent 时重置）
    #[serde(default)]
    pub restart_count: u32,
    /// 最近一次进程退出码（None 表示从未退出或仍在运行）
    #[serde(default)]
    pub last_exit_code: Option<i32>,
}

/// 共享的 Agent 运行时存储。所有字段都是 Arc<Mutex>，
/// 以便在监控线程、自动重启线程中安全共享。
pub struct AgentStore {
    pub agents: Arc<Mutex<HashMap<String, AgentState>>>,
    pub logs: Arc<Mutex<HashMap<String, Vec<LogEntry>>>>,
    pub processes: Arc<Mutex<HashMap<String, Child>>>,
    /// 正在重启中的 agent id → 已重启次数，避免重启风暴与重复触发。
    /// 值为 u32::MAX 时表示被 stop_agent 显式阻止重启。
    pub restarting: Arc<Mutex<HashMap<String, u32>>>,
}

impl AgentStore {
    pub fn new() -> Self {
        Self {
            agents: Arc::new(Mutex::new(HashMap::new())),
            logs: Arc::new(Mutex::new(HashMap::new())),
            processes: Arc::new(Mutex::new(HashMap::new())),
            restarting: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// 自动重启策略常量
pub mod restart_policy {
    /// 启动后存活检查等待时长
    pub const HEALTH_CHECK_MS: u64 = 800;
    /// 单个 agent 最大自动重启次数
    pub const MAX_RESTARTS: u32 = 5;
    /// 两次重启之间的最小退避间隔
    pub const RESTART_BACKOFF_MS: u64 = 1500;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}
