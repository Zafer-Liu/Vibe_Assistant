//! 云端记忆库测试脚手架：一个最小 HTTP/WebDAV 服务。
//!
//! `Remote` 的实现内部直接调用 reqwest，无法在 trait 上打桩，因此把两个模块
//! 都要用的假服务端抽到这里：`vault_remote` 验证条件写时序，`cloud_sync`
//! 验证信封合并（两台设备抢写）。行为对齐 RFC 4918 的条件写语义。

/// 一个只认 PUT / GET / HEAD / MKCOL 的最小 WebDAV 服务。
pub mod fake_dav {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 信封在服务端上的路径，`/{VAULT_FOLDER}/{VAULT_FILE}`。
    pub const VAULT_PATH: &str = "/agent-manager/vault.amv";

    pub struct Server {
        pub url: String,
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        /// 置为 true 后 PUT 一律回 500：模拟「拉取成功但写入失败」。
        fail_put: Arc<AtomicBool>,
        /// 下一次 PUT 强制返回这个状态码（用完即清），用于复现 412 抢写。
        next_put_status: Arc<Mutex<Option<u16>>>,
    }

    impl Server {
        pub fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("绑定回环地址失败");
            let addr = listener.local_addr().expect("读取监听地址失败");
            let files: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::new(Mutex::new(HashMap::new()));
            let etags: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
            let fail_put = Arc::new(AtomicBool::new(false));
            let next_put_status = Arc::new(Mutex::new(None));
            let etag_seq = Arc::new(AtomicUsize::new(0));
            let (f, e, fp, ns, es) = (
                files.clone(),
                etags.clone(),
                fail_put.clone(),
                next_put_status.clone(),
                etag_seq.clone(),
            );
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if let Ok(stream) = stream {
                        handle(stream, &f, &e, &fp, &ns, &es);
                    }
                }
            });
            Self {
                url: format!("http://{addr}"),
                files,
                fail_put,
                next_put_status,
            }
        }

        pub fn body(&self, path: &str) -> Option<Vec<u8>> {
            self.files.lock().unwrap().get(path).cloned()
        }

        /// 让后续 PUT 全部失败，用来复现「信封没写上去但本地已回写」的场景。
        pub fn fail_puts(&self) {
            self.fail_put.store(true, Ordering::SeqCst);
        }

        /// 下一次 PUT 强制返回指定状态码：412 用来复现「写完前被别人抢先」。
        pub fn fail_next_put(&self, status: u16) {
            *self.next_put_status.lock().unwrap() = Some(status);
        }

        pub fn allow_puts(&self) {
            self.fail_put.store(false, Ordering::SeqCst);
        }
    }

    fn write(stream: &mut TcpStream, status: &str, extra: &str, body: &[u8]) {
        write_len(stream, status, extra, body.len(), body);
    }

    /// HEAD 只回头部：真写了 body 会让 keep-alive 上的下一个响应错位。
    fn write_head(stream: &mut TcpStream, status: &str, extra: &str, len: usize) {
        write_len(stream, status, extra, len, &[]);
    }

    fn write_len(
        stream: &mut TcpStream,
        status: &str,
        extra: &str,
        len: usize,
        body: &[u8],
    ) {
        let head = format!("HTTP/1.1 {status}\r\nContent-Length: {len}\r\n{extra}\r\n");
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn handle(
        mut stream: TcpStream,
        files: &Arc<Mutex<HashMap<String, Vec<u8>>>>,
        etags: &Arc<Mutex<HashMap<String, String>>>,
        fail_put: &Arc<AtomicBool>,
        next_put_status: &Arc<Mutex<Option<u16>>>,
        etag_seq: &Arc<AtomicUsize>,
    ) {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            let n = match stream.read(&mut tmp) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let len: usize = head
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        while buf.len() < head_end + len {
            match stream.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let body = buf[head_end..].to_vec();
        let mut lines = head.lines();
        let request = lines.next().unwrap_or("").to_string();
        let mut parts = request.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let target = parts.next().unwrap_or("").to_string();
        let headers: HashMap<String, String> = lines
            .filter_map(|line| {
                line.split_once(':')
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();

        let exists = files.lock().unwrap().contains_key(&target);
        match method.as_str() {
            "PUT" => {
                let forced = next_put_status.lock().unwrap().take();
                let failing = fail_put.load(Ordering::SeqCst);
                if let Some(status) = forced {
                    return write(&mut stream, &format!("{status} Forced"), "", b"");
                }
                if failing {
                    return write(&mut stream, "500 Internal Server Error", "", b"");
                }
                let current = etags.lock().unwrap().get(&target).cloned();
                if let Some(want) = headers.get("if-match") {
                    if current.as_deref() != Some(normalize(want)) {
                        return write(&mut stream, "412 Precondition Failed", "", b"");
                    }
                }
                if headers.get("if-none-match").is_some() && exists {
                    return write(&mut stream, "412 Precondition Failed", "", b"");
                }
                // ETag 必须每次都不同：两次同长度写入若撞号，过期 ETag
                // 反而会被判成有效，条件写测试就失去意义。
                let tag = format!("etag-{}", etag_seq.fetch_add(1, Ordering::SeqCst));
                files.lock().unwrap().insert(target.clone(), body);
                etags.lock().unwrap().insert(target, tag.clone());
                write(&mut stream, "201 Created", &format!("ETag: \"{tag}\"\r\n"), b"")
            }
            "GET" => {
                if !exists {
                    return write(&mut stream, "404 Not Found", "", b"");
                }
                let data = files.lock().unwrap().get(&target).cloned().unwrap_or_default();
                let tag = etags.lock().unwrap().get(&target).cloned().unwrap_or_default();
                write(
                    &mut stream,
                    "200 OK",
                    &format!("ETag: \"{tag}\"\r\n"),
                    &data,
                )
            }
            "HEAD" => {
                if !exists {
                    return write(&mut stream, "404 Not Found", "", b"");
                }
                let len = files.lock().unwrap().get(&target).map(|v| v.len()).unwrap_or(0);
                let tag = etags.lock().unwrap().get(&target).cloned().unwrap_or_default();
                write_head(&mut stream, "200 OK", &format!("ETag: \"{tag}\"\r\n"), len)
            }
            "MKCOL" => write(&mut stream, "201 Created", "", b""),
            _ => write(&mut stream, "405 Method Not Allowed", "", b""),
        }
    }

    fn normalize(raw: &str) -> &str {
        raw.trim().trim_start_matches("W/").trim_matches('"')
    }
}
