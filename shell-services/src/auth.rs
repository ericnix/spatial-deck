//! Autenticação do desbloqueio.
//!
//! Usa o helper setuid `unix_chkpwd` (o mesmo que o `pam_unix` chama para
//! verificar a senha do usuário atual): não precisa de root, não exige
//! `/etc/pam.d/spatial-deck` e **não** passa pelo `pam_faillock`, então uma
//! senha errada no lock screen não arrisca bloquear a conta na sessão host.
//!
//! O protocolo do helper é interno ao pam_unix: a senha vai no stdin
//! terminada em NUL (ver `pam_read_passwords` em `libpam/include/pam_inline.h`).
//! A senha nunca é logada nem persistida.

use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use tokio::{io::AsyncWriteExt, process::Command};
use tracing::warn;

const HELPER: &str = "/usr/sbin/unix_chkpwd";
const MAX_PASSWORD: usize = 512;
const MAX_DELAY: Duration = Duration::from_secs(30);
const TIMEOUT: Duration = Duration::from_secs(5);

/// Estado do backoff entre tentativas.
pub struct AuthGate {
    failures: u32,
    blocked_until: Option<Instant>,
}

impl Default for AuthGate {
    fn default() -> Self {
        Self {
            failures: 0,
            blocked_until: None,
        }
    }
}

impl AuthGate {
    /// Espera após a n-ésima falha: 1, 2, 4, 8, 16, 30, 30… segundos.
    fn delay(failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(5);
        let seconds = 1u64 << exponent;
        Duration::from_secs(seconds.min(MAX_DELAY.as_secs()))
    }

    fn blocked_for(&self, now: Instant) -> Option<Duration> {
        self.blocked_until
            .and_then(|until| until.checked_duration_since(now))
    }

    /// Tenta desbloquear. Devolve `(ok, mensagem)` para o frontend.
    pub async fn attempt(&mut self, password: &str) -> (bool, String) {
        let now = Instant::now();
        if let Some(remaining) = self.blocked_for(now) {
            return (
                false,
                format!(
                    "Aguarde {} s antes de tentar de novo.",
                    remaining.as_secs().max(1)
                ),
            );
        }
        if password.is_empty() {
            return (false, "Digite a senha.".to_owned());
        }
        if password.len() > MAX_PASSWORD {
            return (false, "Senha muito longa.".to_owned());
        }

        if verify_password(password).await {
            self.failures = 0;
            self.blocked_until = None;
            (true, "Desbloqueado.".to_owned())
        } else {
            self.failures = self.failures.saturating_add(1);
            let delay = Self::delay(self.failures);
            self.blocked_until = Some(now + delay);
            warn!(
                failures = self.failures,
                delay_seconds = delay.as_secs(),
                "senha de desbloqueio incorreta"
            );
            (
                false,
                format!(
                    "Senha incorreta. Nova tentativa em {} s.",
                    delay.as_secs()
                ),
            )
        }
    }
}

async fn verify_password(password: &str) -> bool {
    let user = match std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) {
        Ok(user) if !user.is_empty() => user,
        _ => {
            warn!("USER/LOGNAME ausente; não é possível autenticar");
            return false;
        }
    };

    let Ok(mut child) = Command::new(HELPER)
        .arg(&user)
        .arg("nonull")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        warn!(helper = HELPER, "helper de senha indisponível");
        return false;
    };

    if let Some(mut stdin) = child.stdin.take() {
        let mut data = password.as_bytes().to_vec();
        data.push(0);
        let _ = stdin.write_all(&data).await;
        let _ = stdin.shutdown().await;
    }

    match tokio::time::timeout(TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            warn!("timeout ao verificar a senha");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(AuthGate::delay(1), Duration::from_secs(1));
        assert_eq!(AuthGate::delay(2), Duration::from_secs(2));
        assert_eq!(AuthGate::delay(3), Duration::from_secs(4));
        assert_eq!(AuthGate::delay(4), Duration::from_secs(8));
        assert_eq!(AuthGate::delay(5), Duration::from_secs(16));
        assert_eq!(AuthGate::delay(6), MAX_DELAY);
        assert_eq!(AuthGate::delay(50), MAX_DELAY);
    }

    #[test]
    fn blocked_until_counts_down() {
        let now = Instant::now();
        let mut gate = AuthGate::default();
        assert_eq!(gate.blocked_for(now), None);
        gate.blocked_until = Some(now + Duration::from_secs(3));
        assert!(gate.blocked_for(now).unwrap() > Duration::from_secs(2));
        assert_eq!(gate.blocked_for(now + Duration::from_secs(4)), None);
    }
}
