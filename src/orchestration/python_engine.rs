//! V52 Python 执行引擎 — 资产层 skill 的子进程执行体。
//!
//! 资产层可演化 Skill（fork 变体 / 编译产出 / 主动学习实验体）统一为 Python
//! 脚本（`skill.py`），经本引擎子进程执行——与 Rust 种子层（builtin）正交：
//! Rust 原语 = 确定性保证；Python 逻辑 = 可演化能力（Blueprint §6.0 V52）。
//!
//! 安全闸门（3 道）：
//! 1. env_clear——去掉 OPENAI_API_KEY 等全部环境变量，只留 PATH/HOME（§1.3 第一闸门：
//!    Python skill 拿不到 LLM 凭证，无法变成隐藏的 LLM-as-judge）；
//! 2. 超时——30s 硬截止，防死循环；
//! 3. cwd=task_dir——脚本相对路径按任务封地解析，写操作受任务目录约束。

use crate::infra::error::TaijiError;
use serde_json::Value as JsonValue;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// Python 脚本执行超时（30s，与 SkillEngine::COMMAND_TIMEOUT_SECS 对齐）。
pub const PYTHON_TIMEOUT_SECS: u64 = 30;

/// 执行资产层 Python skill 脚本。
///
/// 协议：stdin 写 params JSON → 脚本 `execute(params)` → stdout 打结果 JSON
/// `{"passed": bool, "detail": String, ...}`。非零退出码 / 非法 JSON / 超时 = Err。
///
/// 脚本侧约定（编译管道模板教学，见 `compile.rs::compile_task_description`）：
/// ```python
/// import sys, json
/// def execute(params):
///     return {"passed": True, "detail": "..."}
/// if __name__ == "__main__":
///     print(json.dumps(execute(json.loads(sys.stdin.read())), ensure_ascii=False))
/// ```
pub async fn run_python_skill(
    script_path: &Path,
    params: &JsonValue,
    task_dir: &Path,
    chain: &[String],
) -> Result<JsonValue, TaijiError> {
    run_python_skill_with_timeout(
        script_path,
        params,
        task_dir,
        Duration::from_secs(PYTHON_TIMEOUT_SECS),
        chain,
    )
    .await
}

/// 内部实现：可注入超时（测试用短超时避免拖慢测试套件）。
async fn run_python_skill_with_timeout(
    script_path: &Path,
    params: &JsonValue,
    task_dir: &Path,
    timeout_dur: Duration,
    chain: &[String],
) -> Result<JsonValue, TaijiError> {
    let interpreter = "python3";

    // 保留最小环境：PATH（子进程能找 python / taiji builtin）+ HOME。
    let path_env = std::env::var("PATH").unwrap_or_default();
    let home_env = std::env::var("HOME").unwrap_or_default();

    // V51：脚本路径统一 canonicalize 成绝对路径（相对路径按进程 cwd = 项目根解析）。
    // 调用方传 ".taiji/knowledge/.../skill.py" 这类相对路径时，若不 absolute 化会被
    // cwd=task_dir 二次拼接（".taiji/tasks/<id>/.taiji/..." 双重嵌套 → can't open file）。
    // cwd=task_dir 只影响脚本内部相对路径（任务封地语义），不影响脚本本体定位；
    // 若脚本不存在（canonicalize 失败）则回退原路径，由 python3 报 "can't open file"。
    let script_path =
        std::fs::canonicalize(script_path).unwrap_or_else(|_| script_path.to_path_buf());

    let mut cmd = Command::new(interpreter);
    cmd.arg(&script_path)
        .current_dir(task_dir)
        .env_clear()
        .env("PATH", &path_env)
        .env("HOME", &home_env);
    // V53 调用链（skill 嵌套护栏）：env_clear 后注入，Python skill 内部
    // `taiji skill` 子命令读它做循环/深度检测。空链不注入。
    if !chain.is_empty() {
        let chain_json =
            serde_json::to_string(chain).unwrap_or_else(|_| "[]".to_string());
        cmd.env("TAIJI_SKILL_CHAIN", &chain_json);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        TaijiError::Other(format!(
            "python engine: failed to spawn {interpreter} {:?}: {e}",
            script_path
        ))
    })?;

    // stdin 写入 params JSON（写完 drop stdin → 脚本读到 EOF）。
    let params_json = serde_json::to_string(params).unwrap_or_else(|_| "{}".to_string());
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(params_json.as_bytes()).await;
        let _ = stdin.write_all(b"\n").await;
        let _ = stdin.flush().await;
    }

    // 等待退出（超时 kill 子进程）。
    let timeout_secs = timeout_dur.as_secs();
    let status = match timeout(timeout_dur, child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(TaijiError::Other(format!(
                "python engine: wait failed: {e}"
            )))
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(TaijiError::Other(format!(
                "python skill timed out after {timeout_secs}s: {:?}",
                script_path
            )));
        }
    };

    // 收集 stdout/stderr（wait 后读，防管道堵塞）。
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_end(&mut stdout).await;
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_end(&mut stderr).await;
    }

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(TaijiError::Other(format!(
            "python skill {:?} exited {}: {}",
            script_path,
            status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&stdout);
    serde_json::from_str::<JsonValue>(stdout.trim()).map_err(|e| {
        TaijiError::Other(format!(
            "python skill {:?} produced invalid JSON: {e}\nstdout: {}",
            script_path,
            stdout.trim().chars().take(500).collect::<String>()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    async fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "taiji_python_engine_{tag}_{}_{}",
            std::process::id(),
            n
        ))
    }

    /// 最小合法脚本：读 stdin JSON，返回 {"passed": bool, "echo": 原样回显}。
    const ECHO_SCRIPT: &str = r#"import sys, json
def execute(params):
    return {"passed": True, "echo": params.get("x", None)}
if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read()))))
"#;

    #[tokio::test]
    async fn run_python_skill_roundtrip() {
        let dir = unique_tmp_dir("roundtrip").await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let script = dir.join("skill.py");
        tokio::fs::write(&script, ECHO_SCRIPT).await.unwrap();

        let out = run_python_skill(&script, &serde_json::json!({"x": 42}), &dir, &[])
            .await
            .expect("python skill must run");
        assert_eq!(out["passed"], true);
        assert_eq!(out["echo"], 42);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    /// V51 回归：调用方传相对路径（相对进程 cwd=项目根，如 `.taiji/knowledge/.../skill.py`），
    /// cwd=task_dir 不能二次拼接——引擎应 canonicalize 成绝对路径定位脚本。
    /// 用 `target/` 子目录模拟「项目根下的相对路径」，task_dir 用独立临时目录。
    #[tokio::test]
    async fn run_python_skill_relative_path_resolves_absolutely() {
        let rel = std::path::Path::new("target").join("taiji_py_engine_regression_skill.py");
        tokio::fs::write(&rel, ECHO_SCRIPT).await.unwrap();

        let task_dir = unique_tmp_dir("regression").await;
        tokio::fs::create_dir_all(&task_dir).await.unwrap();

        let out = run_python_skill(&rel, &serde_json::json!({"x": 7}), &task_dir, &[])
            .await
            .expect("relative-to-project-root script must resolve absolutely");
        assert_eq!(out["passed"], true);
        assert_eq!(out["echo"], 7);

        let _ = tokio::fs::remove_file(&rel).await;
        let _ = tokio::fs::remove_dir_all(&task_dir).await;
    }

    #[tokio::test]
    async fn run_python_skill_env_cleared_no_api_key() {
        let dir = unique_tmp_dir("env").await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let script = dir.join("skill.py");
        // 脚本尝试读 OPENAI_API_KEY——env_clear 后应 KeyError（§1.3 第一闸门）。
        tokio::fs::write(
            &script,
            r#"import sys, os, json
def execute(params):
    try:
        key = os.environ["OPENAI_API_KEY"]
        return {"passed": False, "detail": f"leaked key: {key[:4]}..."}
    except KeyError:
        return {"passed": True, "detail": "no api key"}
if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read()))))
"#,
        )
        .await
        .unwrap();

        // 父进程环境里故意放一个 key，验证子进程看不到（Rust 2024 环境变量
        // 写入是 unsafe——测试内单点操作，无并发环境变量读写）。
        let prev = std::env::var("OPENAI_API_KEY").ok();
        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-test-should-not-leak") };

        let out = run_python_skill(&script, &serde_json::json!({}), &dir, &[])
            .await
            .expect("python skill must run");
        assert_eq!(out["passed"], true, "env_clear must hide OPENAI_API_KEY: {out}");
        assert!(out["detail"].as_str().unwrap_or("").contains("no api key"));

        match prev {
            Some(v) => unsafe { std::env::set_var("OPENAI_API_KEY", v) },
            None => unsafe { std::env::remove_var("OPENAI_API_KEY") },
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_python_skill_timeout() {
        let dir = unique_tmp_dir("timeout").await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let script = dir.join("skill.py");
        tokio::fs::write(&script, "import time\ntime.sleep(60)\n").await.unwrap();

        let start = std::time::Instant::now();
        // 测试用 1s 超时（公开 API 默认 30s）——避免拖慢测试套件。
        let out = run_python_skill_with_timeout(
            &script,
            &serde_json::json!({}),
            &dir,
            Duration::from_secs(1),
            &[],
        )
        .await;
        assert!(out.is_err(), "sleep(60) must time out");
        assert!(out.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed().as_secs() < 10, "1s timeout must cut off fast");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_python_skill_nonzero_exit_and_bad_json() {
        let dir = unique_tmp_dir("bad").await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        // 非零退出码。
        let script = dir.join("fail.py");
        tokio::fs::write(&script, "import sys\nsys.exit(3)\n").await.unwrap();
        let out = run_python_skill(&script, &serde_json::json!({}), &dir, &[]).await;
        assert!(out.is_err());
        assert!(out.unwrap_err().to_string().contains("exited 3"));

        // 非法 JSON stdout。
        let script2 = dir.join("badjson.py");
        tokio::fs::write(&script2, "print('not json')\n").await.unwrap();
        let out = run_python_skill(&script2, &serde_json::json!({}), &dir, &[]).await;
        assert!(out.is_err());
        assert!(out.unwrap_err().to_string().contains("invalid JSON"));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn run_python_skill_injects_chain_env() {
        let dir = unique_tmp_dir("chain").await;
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let script = dir.join("skill.py");
        tokio::fs::write(
            &script,
            r#"import sys, os, json
def execute(params):
    return {"passed": True, "chain": os.environ.get("TAIJI_SKILL_CHAIN", "")}
if __name__ == "__main__":
    print(json.dumps(execute(json.loads(sys.stdin.read()))))
"#,
        )
        .await
        .unwrap();

        let chain = vec!["a".to_string(), "b".to_string()];
        let out = run_python_skill(&script, &serde_json::json!({}), &dir, &chain)
            .await
            .expect("python skill must run");
        assert_eq!(out["passed"], true);
        assert_eq!(
            out["chain"],
            serde_json::json!("[\"a\",\"b\"]"),
            "chain env injected as JSON string"
        );

        // 空链不注入（子进程读不到 TAIJI_SKILL_CHAIN）
        let out2 = run_python_skill(&script, &serde_json::json!({}), &dir, &[])
            .await
            .expect("python skill must run");
        assert_eq!(out2["chain"], serde_json::json!(""));

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
