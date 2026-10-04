//! Token-protected, read-only mobile status page for the local network.
//!
//! This is deliberately separate from the agent HTTP API: port 9420 contains
//! mutation routes, while this server exposes only a small status snapshot.

use serde::Serialize;
use serde_json::json;
use std::net::UdpSocket;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MOBILE_STATUS_PORT: u16 = 9421;
const SETTING_ENABLED: &str = "mobile_status.enabled";
const SETTING_TOKEN: &str = "mobile_status.token";

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct MobileStatusSettings {
    pub enabled: bool,
    pub url: String,
    pub port: u16,
    pub token_set: bool,
}

fn setting_enabled() -> bool {
    crate::telemetry_store::shared_store()
        .and_then(|store| store.app_setting_get::<bool>(SETTING_ENABLED))
        .unwrap_or(false)
}

fn ensure_token() -> Result<String, String> {
    let store = crate::telemetry_store::shared_store().ok_or("telemetry store not ready")?;
    if let Some(token) = store
        .app_setting_get::<String>(SETTING_TOKEN)
        .filter(|token| token.len() >= 32)
    {
        return Ok(token);
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    store.app_setting_set(SETTING_TOKEN, &token)?;
    Ok(token)
}

fn local_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("8.8.8.8:80")?;
            socket.local_addr()
        })
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

fn settings_view() -> Result<MobileStatusSettings, String> {
    let token = ensure_token()?;
    Ok(MobileStatusSettings {
        enabled: setting_enabled(),
        url: format!("http://{}:{MOBILE_STATUS_PORT}/?token={token}", local_ip()),
        port: MOBILE_STATUS_PORT,
        token_set: true,
    })
}

#[tauri::command]
pub fn mobile_status_get_settings() -> Result<MobileStatusSettings, String> {
    settings_view()
}

#[tauri::command]
pub fn mobile_status_set_enabled(enabled: bool) -> Result<MobileStatusSettings, String> {
    let store = crate::telemetry_store::shared_store().ok_or("telemetry store not ready")?;
    let _ = ensure_token()?;
    store.app_setting_set(SETTING_ENABLED, &enabled)?;
    settings_view()
}

#[tauri::command]
pub fn mobile_status_rotate_token() -> Result<MobileStatusSettings, String> {
    let store = crate::telemetry_store::shared_store().ok_or("telemetry store not ready")?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    store.app_setting_set(SETTING_TOKEN, &token)?;
    settings_view()
}

pub async fn start_mobile_status_server() {
    let address = format!("0.0.0.0:{MOBILE_STATUS_PORT}");
    let listener = loop {
        match tokio::net::TcpListener::bind(&address).await {
            Ok(listener) => {
                eprintln!("[mobile-status] listening on http://{address} (disabled until enabled)");
                break listener;
            }
            Err(error) => {
                eprintln!(
                    "[mobile-status] failed to bind {address}: {error}; retrying in 5 seconds"
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(handle_connection(stream));
        }
    }
}

async fn handle_connection(mut stream: tokio::net::TcpStream) {
    let mut buffer = [0_u8; 8192];
    let count = match stream.read(&mut buffer).await {
        Ok(count) => count,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buffer[..count]);
    let Some(first_line) = request.lines().next() else {
        return;
    };
    let parts = first_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 2 || parts[0] != "GET" {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "read only",
        )
        .await;
        return;
    }
    let (path, query) = parts[1].split_once('?').unwrap_or((parts[1], ""));
    if !setting_enabled() {
        write_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found",
        )
        .await;
        return;
    }
    let supplied = query_value(query, "token").unwrap_or_default();
    let expected = ensure_token().unwrap_or_default();
    if supplied.len() != expected.len()
        || !constant_time_eq(supplied.as_bytes(), expected.as_bytes())
    {
        write_response(
            &mut stream,
            "401 Unauthorized",
            "text/plain; charset=utf-8",
            "invalid access token",
        )
        .await;
        return;
    }
    match path {
        "/" => {
            write_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                MOBILE_PAGE,
            )
            .await
        }
        "/api/status" => {
            let body = status_snapshot().to_string();
            write_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                &body,
            )
            .await;
        }
        _ => {
            write_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found",
            )
            .await
        }
    }
}

async fn write_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) {
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.flush().await;
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (name, value) = part.split_once('=')?;
        (name == key).then_some(value)
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn status_snapshot() -> serde_json::Value {
    let mut tasks = crate::agent_http::list_agent_tasks();
    tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let tasks = tasks
        .into_iter()
        .take(50)
        .map(|task| {
            json!({
                "id": task.task_id,
                "agent": task.agent_id,
                "title": task.task,
                "status": task.status,
                "created_at": task.created_at,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "generated_at": chrono::Utc::now().timestamp_millis(),
        "tasks": tasks,
    })
}

const MOBILE_PAGE: &str = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Vibe Assistant · 状态</title>
<style>
:root{color-scheme:light dark;font-family:ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif;background:#f6f7f9;color:#18212f}*{box-sizing:border-box}body{margin:0;min-height:100vh;background:#f6f7f9}.shell{max-width:720px;margin:auto;padding:max(22px,env(safe-area-inset-top)) 18px max(28px,env(safe-area-inset-bottom))}header{display:flex;align-items:flex-start;justify-content:space-between;gap:16px;margin-bottom:22px}h1{font-size:22px;letter-spacing:-.025em;margin:0}header p{margin:5px 0 0;color:#697586;font-size:13px}.live{display:flex;align-items:center;gap:7px;font-size:12px;color:#20744a;background:#e7f7ee;padding:7px 10px;border-radius:999px}.dot{width:7px;height:7px;border-radius:50%;background:#2dac68}.attention{display:none;background:#fff5dc;border:1px solid #f2cf73;border-radius:14px;padding:16px;margin-bottom:18px}.attention.show{display:block}.attention strong{display:block;color:#875b00;font-size:15px}.attention span{display:block;color:#96722d;font-size:13px;margin-top:5px}.summary{display:grid;grid-template-columns:repeat(3,1fr);gap:10px;margin-bottom:22px}.metric{background:white;border:1px solid #e3e7ed;border-radius:13px;padding:14px}.metric b{display:block;font-size:23px}.metric span{font-size:11px;color:#697586}section{margin-top:22px}.section-head{display:flex;align-items:center;justify-content:space-between;margin-bottom:9px}.section-head h2{font-size:14px;margin:0}.section-head span{font-size:11px;color:#7a8594}.list{display:grid;gap:8px}.item{background:white;border:1px solid #e3e7ed;border-radius:13px;padding:13px 14px}.row{display:flex;align-items:center;justify-content:space-between;gap:12px}.name{min-width:0;font-size:13px;font-weight:650;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.meta{font-size:11px;color:#7a8594;margin-top:6px;display:flex;gap:8px}.badge{flex:none;border-radius:999px;padding:4px 8px;font-size:10px;font-weight:700;background:#eef1f5;color:#536172}.badge.running{background:#e9f2ff;color:#2869bd}.badge.waiting_acceptance,.badge.blocked,.badge.timeout,.badge.dispatch_failed{background:#fff0ca;color:#895b00}.badge.failed{background:#ffe8e8;color:#ad3333}.badge.success,.badge.closed,.badge.submitted{background:#e7f7ee;color:#20744a}.empty{padding:24px;text-align:center;color:#8a94a3;font-size:13px;border:1px dashed #d5dae2;border-radius:13px}.foot{margin-top:24px;color:#8a94a3;font-size:11px;text-align:center}.error{color:#ad3333}.pulse{animation:pulse .7s ease-out}@keyframes pulse{50%{transform:scale(1.012)}}
@media(prefers-color-scheme:dark){:root,body{background:#0b0e13;color:#eef2f7}.live{background:#102b1e;color:#71d49b}.attention{background:#2a210e;border-color:#654d17}.attention strong{color:#ffd875}.attention span{color:#d8b969}.metric,.item{background:#131820;border-color:#272e39}.metric span,.meta,header p,.section-head span,.foot{color:#8e99aa}.badge{background:#252c36;color:#bac3cf}.badge.running{background:#142a43;color:#7db8fb}.badge.waiting_acceptance,.badge.blocked,.badge.timeout,.badge.dispatch_failed{background:#33280f;color:#f1c75e}.badge.failed{background:#351818;color:#ff9797}.badge.success,.badge.closed,.badge.submitted{background:#112b1e;color:#72d49b}.empty{border-color:#303744;color:#8e99aa}}
</style></head><body><main class="shell"><header><div><h1>Vibe Assistant</h1><p>移动只读状态 · 每 5 秒更新</p></div><div class="live"><span class="dot"></span><span id="live">连接中</span></div></header><div id="attention" class="attention"><strong id="attention-title"></strong><span>打开桌面端处理阻塞或失败任务。</span></div><div class="summary"><div class="metric"><b id="running">—</b><span>执行中</span></div><div class="metric"><b id="waiting">—</b><span>需要输入</span></div><div class="metric"><b id="done">—</b><span>已完成</span></div></div><section><div class="section-head"><h2>智能体任务</h2><span id="task-count"></span></div><div id="tasks" class="list"></div></section><div id="foot" class="foot"></div></main>
<script>
const token=new URLSearchParams(location.search).get('token')||'';let previousAttention=null;
const labels={running:'执行中',success:'成功',failed:'失败',blocked:'阻塞',waiting_acceptance:'待验收',closed:'已关闭',dispatched:'已派发',submitted:'已提交',timeout:'超时',dispatch_failed:'派发失败'};
const attentionStates=new Set(['waiting_acceptance','blocked','failed','timeout','dispatch_failed']);
const time=v=>v?new Intl.DateTimeFormat(undefined,{month:'short',day:'numeric',hour:'2-digit',minute:'2-digit'}).format(new Date(v)):'—';
function empty(text){const e=document.createElement('div');e.className='empty';e.textContent=text;return e}
function renderList(rootId,items){const root=document.getElementById(rootId);root.replaceChildren();if(!items.length){root.append(empty('暂无记录'));return}for(const item of items){const card=document.createElement('div');card.className='item';const row=document.createElement('div');row.className='row';const name=document.createElement('div');name.className='name';name.textContent=item.title||item.id;const badge=document.createElement('span');badge.className='badge '+item.status;badge.textContent=labels[item.status]||item.status;row.append(name,badge);const meta=document.createElement('div');meta.className='meta';meta.textContent=(item.agent?item.agent+' · ':'')+time(item.created_at)+(item.step_count!=null?' · '+item.step_count+' 步':'');card.append(row,meta);root.append(card)}}
async function refresh(){try{const response=await fetch('/api/status?token='+encodeURIComponent(token),{cache:'no-store'});if(!response.ok)throw new Error('HTTP '+response.status);const data=await response.json();const all=[...data.tasks];const attention=all.filter(item=>attentionStates.has(item.status)).length;const running=all.filter(item=>item.status==='running'||item.status==='dispatched').length;const done=all.filter(item=>['success','closed','submitted'].includes(item.status)).length;document.getElementById('running').textContent=running;document.getElementById('waiting').textContent=attention;document.getElementById('done').textContent=done;document.getElementById('task-count').textContent=data.tasks.length+' 条';document.getElementById('live').textContent='在线';document.getElementById('foot').textContent='最近更新 '+time(data.generated_at);const panel=document.getElementById('attention');panel.classList.toggle('show',attention>0);document.getElementById('attention-title').textContent=attention+' 项需要你的输入';if(previousAttention!==null&&attention>previousAttention){panel.classList.remove('pulse');requestAnimationFrame(()=>panel.classList.add('pulse'));if(navigator.vibrate)navigator.vibrate([120,80,120])}previousAttention=attention;document.title=(attention?'('+attention+') ':'')+'Vibe Assistant · 状态';renderList('tasks',data.tasks)}catch(error){document.getElementById('live').textContent='连接失败';document.getElementById('live').parentElement.classList.add('error');document.getElementById('foot').textContent=String(error)}}
refresh();setInterval(refresh,5000);
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_token_is_exact() {
        assert_eq!(query_value("a=1&token=abc&b=2", "token"), Some("abc"));
        assert_eq!(query_value("access_token=abc", "token"), None);
    }

    #[test]
    fn token_comparison_checks_every_byte() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
