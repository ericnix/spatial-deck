//! Cliente do socket IPC nativo do compositor, com reconexão automática.

use std::time::Duration;

use anyhow::{Context, Result};
use common::{IpcMessage, IPC_SOCKET_PATH};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    sync::mpsc,
};
use tracing::{info, warn};

/// Encaminha mensagens entre o canal de saída e o compositor. Reconecta para
/// sempre: o serviço pode subir antes do compositor e continuar de pé quando
/// ele reinicia.
pub async fn run(mut outgoing: mpsc::Receiver<IpcMessage>, incoming: mpsc::Sender<IpcMessage>) {
    loop {
        match connect(&mut outgoing, &incoming).await {
            Ok(()) => info!("conexão IPC com o compositor encerrada"),
            Err(error) => warn!(%error, "falha na conexão IPC"),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn connect(
    outgoing: &mut mpsc::Receiver<IpcMessage>,
    incoming: &mpsc::Sender<IpcMessage>,
) -> Result<()> {
    let stream = UnixStream::connect(IPC_SOCKET_PATH)
        .await
        .with_context(|| format!("conectar em {IPC_SOCKET_PATH}"))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    info!(socket = IPC_SOCKET_PATH, "conectado ao compositor");

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    return Ok(());
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match IpcMessage::from_json(line) {
                    Ok(message) => {
                        if incoming.send(message).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(error) => warn!(%error, "mensagem IPC inválida do compositor"),
                }
            }
            message = outgoing.recv() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let json = message.to_json()?;
                write_half.write_all(json.as_bytes()).await?;
                write_half.write_all(b"\n").await?;
            }
        }
    }
}
