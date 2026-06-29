// apicase 后端：单 API 调试核心命令 send_request。
// 模型与执行逻辑分离 —— perform_request 不依赖 Tauri，可独立单元/集成测试。
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 一对 HTTP 头（请求头 / 响应头通用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
}

/// 前端传入的 API 请求 —— 即「单节点 DAG」的执行输入
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<HeaderEntry>,
    #[serde(default)]
    pub body: Option<String>,
}

/// 返回给前端的响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<HeaderEntry>,
    pub body: String,
    pub elapsed_ms: u128,
}

/// 真正发起请求的逻辑（与 Tauri 解耦，便于测试）。
/// 由后端用 reqwest 发出，天然绕过浏览器 CORS。
async fn perform_request(req: ApiRequest) -> Result<ApiResponse, String> {
    let url = req.url.trim();
    if url.is_empty() {
        return Err("URL 不能为空".to_string());
    }

    let method = reqwest::Method::from_bytes(req.method.trim().to_uppercase().as_bytes())
        .map_err(|_| format!("非法的 HTTP 方法: {}", req.method))?;

    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut builder = client.request(method, url);
    for h in &req.headers {
        if h.key.trim().is_empty() {
            continue;
        }
        builder = builder.header(h.key.trim(), h.value.as_str());
    }
    if let Some(body) = &req.body {
        if !body.is_empty() {
            builder = builder.body(body.clone());
        }
    }

    let start = Instant::now();
    let resp = builder
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;

    let status = resp.status();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let headers: Vec<HeaderEntry> = resp
        .headers()
        .iter()
        .map(|(k, v)| HeaderEntry {
            key: k.to_string(),
            value: v.to_str().unwrap_or("").to_string(),
        })
        .collect();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("读取响应体失败: {e}"))?;
    let elapsed_ms = start.elapsed().as_millis();

    Ok(ApiResponse {
        status: status.as_u16(),
        status_text,
        headers,
        body,
        elapsed_ms,
    })
}

/// Tauri 命令：发送单个 API 请求
#[tauri::command]
async fn send_request(request: ApiRequest) -> Result<ApiResponse, String> {
    perform_request(request).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![send_request])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 含空格的非法方法应在发起前报错（无需联网）
    #[tokio::test]
    async fn invalid_method_is_rejected() {
        let req = ApiRequest {
            method: "BAD METHOD".into(),
            url: "https://example.com".into(),
            headers: vec![],
            body: None,
        };
        assert!(perform_request(req).await.is_err());
    }

    /// 空 URL 应报错（无需联网）
    #[tokio::test]
    async fn empty_url_is_rejected() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "   ".into(),
            headers: vec![],
            body: None,
        };
        assert!(perform_request(req).await.is_err());
    }

    /// 真实 GET：验证 单节点 DAG 端到端链路（需联网）
    #[tokio::test]
    async fn real_get_request_succeeds() {
        let req = ApiRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            headers: vec![],
            body: None,
        };
        let resp = perform_request(req).await.expect("请求应成功");
        assert_eq!(resp.status, 200);
        assert!(!resp.body.is_empty());
    }
}
