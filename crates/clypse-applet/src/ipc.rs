/// Cliente IPC one-shot do applet — conecta ao daemon, pede itens, desconecta.
///
/// Diferente da GUI (conexão persistente), o applet busca o histórico a cada
/// abertura do popup e a cada mudança na busca. Se o daemon não estiver
/// rodando, ele é iniciado aqui mesmo (sem systemd — funciona no Flatpak).
use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct ClipItem {
    pub content:     Option<String>,
    pub mime_type:   String,
    pub is_favorite: bool,
    pub is_pinned:   bool,
}

pub fn socket_path() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("clypse/daemon.sock")
}

/// Busca itens do daemon; inicia o daemon se necessário.
/// Retorna lista vazia em caso de falha (o popup mostra estado vazio).
pub async fn fetch_items(search: Option<String>) -> Vec<ClipItem> {
    match try_fetch(search).await {
        Ok(items) => items,
        Err(e) => {
            warn!("IPC fetch failed: {}", e);
            Vec::new()
        }
    }
}

async fn try_fetch(search: Option<String>) -> Result<Vec<ClipItem>> {
    let path = socket_path();

    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => {
            start_daemon()?;
            connect_with_retry(&path).await?
        }
    };

    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let search_json = match &search {
        Some(s) => serde_json::to_string(s)?,
        None    => "null".into(),
    };
    let req = format!(
        "{{\"method\":\"get_items\",\"id\":1,\"search\":{},\"limit\":40,\"offset\":0}}\n",
        search_json
    );
    writer.write_all(req.as_bytes()).await?;

    // Lê linhas até a resposta do nosso request (eventos push são ignorados)
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let line = tokio::time::timeout_at(deadline, lines.next_line())
            .await
            .map_err(|_| anyhow!("daemon response timed out"))??
            .ok_or_else(|| anyhow!("daemon closed connection"))?;

        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v["type"] == "result" && v["data"]["kind"] == "items" {
            let items: Vec<ClipItem> =
                serde_json::from_value(v["data"]["items"].clone()).unwrap_or_default();
            return Ok(items);
        }
        if v["type"] == "error" {
            return Err(anyhow!("daemon error: {}", v["message"]));
        }
    }
}

async fn connect_with_retry(path: &PathBuf) -> Result<UnixStream> {
    for _ in 0..30 {
        if let Ok(s) = UnixStream::connect(path).await {
            return Ok(s);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(anyhow!("daemon did not come up at {}", path.display()))
}

fn start_daemon() -> Result<()> {
    // Fora do Flatpak, prefere o serviço systemd (instalações nativas)
    if std::env::var_os("FLATPAK_ID").is_none() {
        if std::process::Command::new("systemctl")
            .args(["--user", "start", "clypse-daemon.service"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            info!("Daemon started via systemd");
            return Ok(());
        }
    }

    let daemon_bin = which::which("clypse-daemon")
        .map_err(|_| anyhow!("clypse-daemon not found. Is it installed?"))?;

    std::process::Command::new(&daemon_bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    info!("Daemon started: {}", daemon_bin.display());
    Ok(())
}
