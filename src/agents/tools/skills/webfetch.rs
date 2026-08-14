//! WebfetchTool — HTTP URL fetcher with SSRF protection.
//!
//! Based on pi_agent_rust's general HTTP extension pattern. Not present in
//! pi_agent core, implemented from scratch for taiji.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use std::path::Path;

use super::BuiltinSkill;
use crate::hooks::safety::SafetyHook;
use crate::infra::error::TaijiError;

/// Built-in `webfetch` skill.
#[derive(Debug, Clone, Default)]
pub struct WebfetchTool;

/// Maximum response body size (512 KB).
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

/// Check for SSRF — delegates to the single source of truth in
/// [`SafetyHook::check_web_url_static`] (AGENTS.md §16 危险隔离), covering
/// decimal/hex private-IP encodings, all RFC1918/link-local ranges, and
/// fragment-before-`@` bypasses that the old local check missed (批10 P1).
fn check_ssrf(url: &str) -> Result<(), TaijiError> {
    SafetyHook::check_web_url_static(url)
}

#[async_trait]
impl BuiltinSkill for WebfetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    async fn call(&self, _task_dir: &Path, args: &JsonValue) -> Result<JsonValue, TaijiError> {
        let url_str = args
            .get("url")
            .and_then(JsonValue::as_str)
            .or_else(|| args.get("input").and_then(JsonValue::as_str))
            .ok_or_else(|| {
                TaijiError::Other("webfetch: missing required 'url' argument".into())
            })?;

        // SSRF check
        check_ssrf(url_str)?;

        // Validate URL format
        let _parsed = url::Url::parse(url_str).map_err(|e| {
            TaijiError::Other(format!("webfetch: invalid URL '{}': {e}", url_str))
        })?;

        // Build client with timeout
        let client = reqwest::Client::builder()
            .user_agent("taiji/0.1.0")
            .timeout(std::time::Duration::from_secs(15))
            // 逐跳 SSRF 检查（批10 P1）：跟随重定向前对每一跳目标做 SSRF 检查，
            // 阻止初始公网 URL 302→内网地址的绕过；最多 5 跳。
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if check_ssrf(attempt.url().as_str()).is_err() {
                    attempt.stop()
                } else if attempt.previous().len() >= 5 {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|e| {
                TaijiError::Other(format!("webfetch: failed to create HTTP client: {e}"))
            })?;

        let response = client.get(url_str).send().await.map_err(|e| {
            TaijiError::Other(format!("webfetch: request failed: {e}"))
        })?;

        let status_code = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        // Read body with size limit
        let body_bytes = response.bytes().await.map_err(|e| {
            TaijiError::Other(format!("webfetch: failed to read response body: {e}"))
        })?;

        let total_bytes = body_bytes.len();
        let truncated = total_bytes > MAX_RESPONSE_BYTES;

        let content = if truncated {
            // Only take first MAX_RESPONSE_BYTES
            let mut bytes = body_bytes.to_vec();
            bytes.truncate(MAX_RESPONSE_BYTES);
            String::from_utf8_lossy(&bytes).to_string()
        } else {
            String::from_utf8_lossy(&body_bytes).to_string()
        };

        let mut result = serde_json::json!({
            "tool": "webfetch",
            "status": "ok",
            "url": url_str,
            "status_code": status_code,
            "content_type": content_type,
            "content": content,
            "total_bytes": total_bytes,
        });

        if truncated {
            result["truncated"] = serde_json::Value::Bool(true);
        }

        if status_code >= 400 {
            result["status"] = serde_json::Value::String("error".into());
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_webfetch_missing_url() {
        let tool = WebfetchTool;
        let args = serde_json::json!({});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_webfetch_ssrf_localhost() {
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "http://127.0.0.1:8080/"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SSRF"));
    }

    #[tokio::test]
    async fn test_webfetch_ssrf_localhost_name() {
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "http://localhost:8080/"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_webfetch_ssrf_decimal_ip() {
        // 167772160 = 10.0.0.0（十进制编码的私网 IP，批10 P1 绕过）
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "http://167772160/"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SSRF"));
    }

    #[tokio::test]
    async fn test_webfetch_ssrf_hex_ip() {
        // 0x7f000001 = 127.0.0.1（十六进制编码的环回地址）
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "http://0x7f000001/"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_webfetch_ssrf_private_10() {
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "http://10.0.0.1/"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_webfetch_invalid_url() {
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "not a url"});
        let result = tool.call(std::path::Path::new("."), &args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore] // requires network
    async fn test_webfetch_real_url() {
        let tool = WebfetchTool;
        let args = serde_json::json!({"url": "https://httpbin.org/get"});
        let result = tool.call(std::path::Path::new("."), &args).await.unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["status_code"], 200);
    }
}
