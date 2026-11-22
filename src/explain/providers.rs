//! Explicit provider dispatch; no fallback and no model-callable tools.
use super::{ExplainError, Result, context::SYSTEM};
use crate::config::AiConfig;
use serde::Serialize;
use serde_json::{Value, json};
use std::{net::IpAddr, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

const MAX_OUTPUT: usize = 128_000;
const CLAUDE_FLAGS: &[&str] = &[
    "--print",
    "--output-format",
    "json",
    "--tools",
    "",
    "--safe-mode",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{}}",
    "--disable-slash-commands",
    "--no-session-persistence",
    "--no-chrome",
    "--setting-sources",
    "",
    "--settings",
    "{\"disableAllHooks\":true}",
    "--permission-mode",
    "dontAsk",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub model: String,
    pub processing: &'static str,
    pub available: bool,
    pub detail: String,
}

pub fn local_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| ExplainError::Invalid("Invalid local Ollama URL."))?;
    // Literal IP only: no DNS rebinding, redirects, credentials, proxy or remote hosts.
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    if url.scheme() != "http"
        || !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(ExplainError::Invalid(
            "Local Ollama URL must be an http loopback IP with no path, credentials or query (for example http://127.0.0.1:11434).",
        ));
    }
    Ok(url)
}

fn model_ok(model: &str) -> bool {
    !model.trim().is_empty()
        && model.len() <= 200
        && !model.starts_with('-')
        && !model.chars().any(char::is_control)
}

async fn claude_supported() -> bool {
    let mut command = Command::new("claude");
    command.arg("--help");
    let output = tokio::time::timeout(Duration::from_secs(5), run_cli(command, None)).await;
    let Ok(Ok(output)) = output else {
        return false;
    };
    [
        "--print",
        "--output-format",
        "--model",
        "--system-prompt",
        "--tools",
        "--safe-mode",
        "--strict-mcp-config",
        "--mcp-config",
        "--no-session-persistence",
        "--disable-slash-commands",
        "--setting-sources",
        "--settings",
        "--permission-mode",
        "--no-chrome",
    ]
    .iter()
    .all(|flag| output.contains(flag))
}

pub async fn list(config: &AiConfig) -> Vec<Provider> {
    let local = model_ok(&config.local_model) && local_url(&config.local_url).is_ok();
    let claude =
        config.claude_enabled && model_ok(&config.claude_model) && claude_supported().await;
    vec![
        Provider { id: "local", label: "Local Ollama", model: config.local_model.clone(), processing: "local", available: local,
            detail: if local { "Loopback only. Server and local model are checked on send; cloud models are refused." } else { "Set ai.local_model and a loopback ai.local_url with aside set, then restart Aside." }.into() },
        Provider { id: "claude", label: "Claude Code", model: config.claude_model.clone(), processing: "hosted", available: claude,
            detail: if claude { "Sends the selected context to your Claude provider. Uses the CLI's existing authentication; tools and customizations are disabled. Charges or subscription limits may apply." } else { "Enable ai.claude_enabled, configure ai.claude_model, and install/authenticate a Claude CLI supporting --safe-mode and --tools. Restart Aside after configuration." }.into() },
        Provider { id: "codex", label: "Codex", model: String::new(), processing: "hosted", available: false,
            detail: "Not supported yet: Codex CLI 0.149.0 has no verified blanket no-tools interface. Read-only sandboxing still allows commands. No request will be sent to Codex.".into() },
    ]
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(85))
        .build()
        .map_err(|_| ExplainError::Provider("Could not create the local HTTP client."))
}

async fn local_post(client: &reqwest::Client, url: reqwest::Url, body: Value) -> Result<Value> {
    let mut response = client.post(url).header("Content-Type", "application/json").body(body.to_string())
        .send().await.map_err(|_| ExplainError::Provider("Local Ollama is unavailable or timed out. Start the server and check the configured port. No hosted fallback was used."))?;
    if !response.status().is_success() {
        return Err(ExplainError::Provider(
            "Local Ollama rejected the request. Check that the model is installed. No hosted fallback was used.",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ExplainError::Provider("Could not read the local response."))?
    {
        if bytes.len() + chunk.len() > MAX_OUTPUT {
            return Err(ExplainError::Provider(
                "Provider response exceeded the size limit.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ExplainError::Provider("Provider returned invalid JSON."))
}

fn is_local_model(info: &Value) -> bool {
    // A real local model has architecture metadata. Fail closed on remote-only or unknown metadata.
    info.get("remote_model")
        .is_none_or(|v| v.as_str() == Some(""))
        && info
            .get("remote_host")
            .is_none_or(|v| v.as_str() == Some(""))
        && info
            .pointer("/model_info/general.architecture")
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty())
}

pub async fn answer(config: &AiConfig, provider: &str, prompt: &str) -> Result<String> {
    match provider {
        "local" => {
            if !model_ok(&config.local_model) {
                return Err(ExplainError::Invalid(
                    "Configure ai.local_model before asking a question.",
                ));
            }
            let base = local_url(&config.local_url)?;
            let client = client()?;
            let info = local_post(
                &client,
                base.join("api/show").unwrap(),
                json!({"model": config.local_model}),
            )
            .await?;
            if !is_local_model(&info) {
                return Err(ExplainError::Invalid(
                    "This is not a verified local Ollama model. Cloud/remote models are not allowed for local Q&A.",
                ));
            }
            let response = local_post(&client, base.join("api/chat").unwrap(), json!({
                "model": config.local_model, "stream": false, "tools": [],
                "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": prompt}],
                "options": {"num_predict": 1500, "num_ctx": 32768}
            })).await?;
            if response.get("done") != Some(&Value::Bool(true))
                || response.get("error").is_some()
                || response
                    .pointer("/message/tool_calls")
                    .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
            {
                return Err(ExplainError::Provider(
                    "Local model did not return a complete text answer. No tools were executed.",
                ));
            }
            text_answer(response.pointer("/message/content").and_then(Value::as_str))
        }
        "claude" => {
            if !config.claude_enabled
                || !model_ok(&config.claude_model)
                || !claude_supported().await
            {
                return Err(ExplainError::Invalid(
                    "Claude is disabled or lacks the required tool-free CLI options. Check setup and restart Aside.",
                ));
            }
            // No repository working directory, shell interpolation, conversation resume, or agent session.
            let dir = tempfile::tempdir().map_err(|_| {
                ExplainError::Provider("Could not create an isolated CLI directory.")
            })?;
            let command = claude_command(&config.claude_model, dir.path());
            let output = run_cli(command, Some(prompt)).await?;
            claude_answer(&output)
        }
        "codex" => Err(ExplainError::Invalid(
            "Codex Q&A is unavailable until a verified tool-free interface is supported.",
        )),
        _ => Err(ExplainError::Invalid("Unknown explanation provider.")),
    }
}

fn claude_command(model: &str, cwd: &std::path::Path) -> Command {
    let mut command = Command::new("claude");
    command
        .args(CLAUDE_FLAGS)
        .arg("--model")
        .arg(model)
        .arg("--system-prompt")
        .arg(SYSTEM)
        .current_dir(cwd);
    command
}

fn claude_answer(output: &str) -> Result<String> {
    let result: Value = serde_json::from_str(output)
        .map_err(|_| ExplainError::Provider("Claude returned invalid JSON."))?;
    if result.get("is_error") == Some(&Value::Bool(true))
        || result.get("subtype").and_then(Value::as_str) != Some("success")
    {
        return Err(ExplainError::Provider(
            "Claude could not answer. Check CLI authentication, model access and quota. No provider fallback was used.",
        ));
    }
    text_answer(result.get("result").and_then(Value::as_str))
}

fn text_answer(text: Option<&str>) -> Result<String> {
    let text = text.unwrap_or("").trim();
    if text.is_empty() || text.len() > 16_000 {
        return Err(ExplainError::Provider(
            "Provider returned an empty or oversized answer.",
        ));
    }
    Ok(text.to_owned())
}

async fn run_cli(mut command: Command, input: Option<&str>) -> Result<String> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|_| {
        ExplainError::Provider("Could not start Claude. Install the CLI and check PATH.")
    })?;
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap().take((MAX_OUTPUT + 1) as u64);
    let write = async {
        if let Some(input) = input {
            stdin.write_all(input.as_bytes()).await?;
        }
        drop(stdin);
        Ok::<_, std::io::Error>(())
    };
    let read = async {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    };
    let (_, bytes) = tokio::try_join!(write, read)
        .map_err(|_| ExplainError::Provider("Could not communicate with Claude."))?;
    if bytes.len() > MAX_OUTPUT {
        return Err(ExplainError::Provider(
            "Claude response exceeded the size limit.",
        ));
    }
    if !child
        .wait()
        .await
        .map_err(|_| ExplainError::Provider("Claude process failed."))?
        .success()
    {
        return Err(ExplainError::Provider(
            "Claude failed. Check CLI authentication, model access and quota. No provider fallback was used.",
        ));
    }
    String::from_utf8(bytes).map_err(|_| ExplainError::Provider("Claude returned invalid text."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_literal_loopback_without_redirect_credentials_or_paths() {
        for url in ["http://127.0.0.1:11434", "http://[::1]:11434/"] {
            assert!(local_url(url).is_ok(), "{url}");
        }
        for url in [
            "https://example.com",
            "http://localhost:11434",
            "http://192.168.1.1",
            "http://user:secret@127.0.0.1",
            "http://127.0.0.1/api/chat",
            "http://127.0.0.1?x=1",
        ] {
            assert!(local_url(url).is_err(), "{url}");
        }
    }
    #[test]
    fn claude_uses_tool_free_isolated_nonpersistent_flags_and_parses_only_success() {
        let cwd = std::path::Path::new("isolated-dir");
        let command = claude_command("sonnet", cwd);
        let command = command.as_std();
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(command.get_current_dir(), Some(cwd));
        assert!(args.windows(2).any(|a| a == ["--tools", ""]));
        assert!(
            args.windows(2)
                .any(|a| a == ["--mcp-config", "{\"mcpServers\":{}}"])
        );
        assert!(
            args.windows(2)
                .any(|a| a == ["--settings", "{\"disableAllHooks\":true}"])
        );
        for flag in [
            "--safe-mode",
            "--strict-mcp-config",
            "--no-session-persistence",
            "--disable-slash-commands",
        ] {
            assert!(args.contains(&flag));
        }
        assert_eq!(
            claude_answer(r#"{"subtype":"success","result":"An explanation","is_error":false}"#)
                .unwrap(),
            "An explanation"
        );
        for output in [
            r#"{"subtype":"error","result":"secret"}"#,
            r#"{"subtype":"success","is_error":true,"result":"secret"}"#,
            "not json",
        ] {
            assert!(claude_answer(output).is_err());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_transport_uses_stdin_and_bounds_output_without_a_live_model() {
        let mut echo = Command::new("sh");
        echo.args(["-c", "cat"]);
        assert_eq!(
            run_cli(echo, Some("synthetic context, not argv"))
                .await
                .unwrap(),
            "synthetic context, not argv"
        );
        let mut fail = Command::new("sh");
        fail.args(["-c", "echo private-secret >&2; exit 1"]);
        let error = run_cli(fail, None).await.unwrap_err();
        assert!(!format!("{error:?}").contains("private-secret"));
        let mut huge = Command::new("sh");
        huge.args(["-c", "head -c 130000 /dev/zero"]);
        assert!(run_cli(huge, None).await.is_err());
    }

    #[test]
    fn cloud_and_unknown_models_fail_closed() {
        let local = json!({"model_info":{"general.architecture":"llama"}});
        assert!(is_local_model(&local));
        for remote in [
            json!({}),
            json!({"remote_host":"https://ollama.com"}),
            json!({"remote_model":"cloud", "model_info":{"general.architecture":"llama"}}),
        ] {
            assert!(!is_local_model(&remote));
        }
    }
}
