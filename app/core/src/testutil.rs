//! 测试用的本地 HTTP 服务（仅 `cfg(test)`）。
//!
//! 执行引擎的价值全在"真的把请求发出去、真的把响应解回来"这条链路上，
//! 用假的 send 回调去测等于把被测对象换掉了。这里起一个真实的 TCP 服务，
//! 手写 HTTP/1.1 的最小子集——比拉一个 mock 框架轻，而且**支持 keep-alive**，
//! 因此还能顺带验证连接复用确实发生了。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 服务端收到的一个请求（供测试断言"到底发了什么"）。
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_ascii_lowercase()).map(String::as_str)
    }
}

/// 一次响应：状态码 + 若干头 + 报文体（+ 可选的服务端延迟）。
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// 应答前的延迟。**必须由服务端异步实现**——handler 里直接
    /// `std::thread::sleep` 会把 tokio 的 worker 线程整个堵住，
    /// 于是"并发"测试测出来的是串行，还看不出哪里错了。
    pub delay_ms: u64,
}

impl Reply {
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            delay_ms: 0,
        }
    }
    pub fn status(code: u16) -> Self {
        Self { status: code, headers: vec![], body: String::new(), delay_ms: 0 }
    }
    pub fn with_header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn with_body(mut self, b: impl Into<String>) -> Self {
        self.body = b.into();
        self
    }
    pub fn after(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

type Handler = Arc<dyn Fn(&Recorded) -> Reply + Send + Sync>;

pub struct MockServer {
    pub base: String,
    log: Arc<Mutex<Vec<Recorded>>>,
    conns: Arc<Mutex<usize>>,
}

impl MockServer {
    /// 起服务并返回其 `http://127.0.0.1:<port>` 基址。任务随 runtime 结束而收摊。
    pub async fn start(handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口");
        let addr = listener.local_addr().expect("取本地地址");
        let log: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let conns = Arc::new(Mutex::new(0usize));
        let handler: Handler = Arc::new(handler);

        let (l2, c2, h2) = (log.clone(), conns.clone(), handler.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else { break };
                *c2.lock().unwrap() += 1;
                let (l, h) = (l2.clone(), h2.clone());
                tokio::spawn(async move { serve_conn(sock, l, h).await });
            }
        });

        Self { base: format!("http://{addr}"), log, conns }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// 累计接受的 TCP 连接数。连接复用生效时，N 个请求只该开 1 条连接。
    pub fn connection_count(&self) -> usize {
        *self.conns.lock().unwrap()
    }
}

/// 一条连接上循环处理请求（keep-alive）。
async fn serve_conn(mut sock: tokio::net::TcpStream, log: Arc<Mutex<Vec<Recorded>>>, handler: Handler) {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    loop {
        // 先把头部读全（到 \r\n\r\n 为止）
        let head_end = loop {
            if let Some(p) = find_double_crlf(&buf) {
                break p;
            }
            let mut chunk = [0u8; 4096];
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };

        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let mut req = parse_head(&head);
        let body_len: usize = req.header("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);

        let body_start = head_end + 4;
        while buf.len() < body_start + body_len {
            let mut chunk = [0u8; 4096];
            match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        req.body = String::from_utf8_lossy(&buf[body_start..body_start + body_len]).into_owned();
        buf.drain(..body_start + body_len);

        let reply = handler(&req);
        log.lock().unwrap().push(req);

        if reply.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(reply.delay_ms)).await;
        }

        let mut out = format!("HTTP/1.1 {} {}\r\n", reply.status, reason(reply.status));
        for (k, v) in &reply.headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        out.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
        out.push_str("Connection: keep-alive\r\n\r\n");
        out.push_str(&reply.body);
        if sock.write_all(out.as_bytes()).await.is_err() {
            return;
        }
    }
}

fn find_double_crlf(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_head(head: &str) -> Recorded {
    let mut lines = head.split("\r\n");
    let mut req = Recorded::default();
    if let Some(first) = lines.next() {
        let mut parts = first.split_whitespace();
        req.method = parts.next().unwrap_or("").to_string();
        req.path = parts.next().unwrap_or("").to_string();
    }
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            req.headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    req
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}
