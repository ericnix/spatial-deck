//! Ações de sessão via logind.
//!
//! `Suspend`, `Reboot` e `PowerOff` chamam o `org.freedesktop.login1.Manager`
//! com `interactive=false`. `Logout` **não** passa por aqui: o compositor
//! encerra só o desktop aninhado. `Lock` é placeholder até a etapa 9.
//!
//! Para validar o caminho sem reiniciar/desligar a máquina de verdade,
//! `SPATIAL_DECK_SESSION_DRYRUN=1` apenas registra a chamada no log.

use std::env;

use common::SessionActionKind;
use tracing::{info, warn};
use zbus::{Connection, Proxy};

const LOGIND_NAME: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

pub async fn perform(action: SessionActionKind) {
    let Some(method) = method_for(action) else {
        info!(?action, "ação de sessão tratada fora do logind");
        return;
    };

    if env::var("SPATIAL_DECK_SESSION_DRYRUN").as_deref() == Ok("1") {
        info!(?action, method, "ação de sessão em dry-run");
        return;
    }

    let Ok(connection) = Connection::system().await else {
        warn!("barramento de sistema indisponível para a ação de sessão");
        return;
    };
    let Ok(proxy) = Proxy::new_owned(connection, LOGIND_NAME, LOGIND_PATH, LOGIND_MANAGER).await
    else {
        warn!("logind indisponível");
        return;
    };

    match proxy.call::<_, _, ()>(method, &(false,)).await {
        Ok(()) => info!(?action, method, "ação de sessão enviada ao logind"),
        Err(error) => warn!(%error, ?action, method, "falha na ação de sessão"),
    }
}

/// Método do logind para cada ação (sem `interactive`).
fn method_for(action: SessionActionKind) -> Option<&'static str> {
    match action {
        SessionActionKind::Suspend => Some("Suspend"),
        SessionActionKind::Reboot => Some("Reboot"),
        SessionActionKind::PowerOff => Some("PowerOff"),
        // Logout/Unlock são tratados no compositor; Lock é estado local.
        SessionActionKind::Logout
        | SessionActionKind::Lock
        | SessionActionKind::Unlock => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_destructive_actions_to_logind() {
        assert_eq!(method_for(SessionActionKind::Suspend), Some("Suspend"));
        assert_eq!(method_for(SessionActionKind::Reboot), Some("Reboot"));
        assert_eq!(method_for(SessionActionKind::PowerOff), Some("PowerOff"));
    }

    #[test]
    fn logout_lock_and_unlock_stay_out_of_logind() {
        assert_eq!(method_for(SessionActionKind::Logout), None);
        assert_eq!(method_for(SessionActionKind::Lock), None);
        assert_eq!(method_for(SessionActionKind::Unlock), None);
    }
}
