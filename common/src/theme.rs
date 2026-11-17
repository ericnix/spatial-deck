//! Temas visuais compartilhados pelo compositor e pelo frontend.
//!
//! O nome serializado é o mesmo usado no atributo `data-canvas-theme` do
//! frontend e no campo `theme` das mensagens IPC:
//!
//! - `default`
//! - `minimalist`
//! - `minimalist-dark`
//! - `oxidized-iron`
//! - `nord`

use serde::{Deserialize, Serialize};

/// Tema aplicado ao canvas, aos cartões e às bordas das janelas nativas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CanvasTheme {
    /// Tema escuro original do Spatial Deck.
    #[default]
    Default,
    /// Modo claro monocromático com grid de papel milimetrado.
    Minimalist,
    /// Modo escuro monocromático: fundo negro, letras e contornos brancos.
    MinimalistDark,
    /// Tema escuro com tons de ferro oxidado.
    OxidizedIron,
    /// Paleta Nord: azul-acinzentado escuro com acentos frost.
    Nord,
}

impl CanvasTheme {
    /// Todos os temas na ordem exibida pelo seletor do HUD.
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Minimalist,
        Self::MinimalistDark,
        Self::OxidizedIron,
        Self::Nord,
    ];

    /// Nome canônico usado no CSS e no protocolo IPC.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Minimalist => "minimalist",
            Self::MinimalistDark => "minimalist-dark",
            Self::OxidizedIron => "oxidized-iron",
            Self::Nord => "nord",
        }
    }

    /// Converte um nome vindo da URL, do `localStorage` ou do IPC.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "default" => Some(Self::Default),
            "minimalist" => Some(Self::Minimalist),
            "minimalist-dark" | "minimalist_dark" => Some(Self::MinimalistDark),
            "oxidized-iron" | "oxidized_iron" => Some(Self::OxidizedIron),
            "nord" => Some(Self::Nord),
            _ => None,
        }
    }
}

impl std::fmt::Display for CanvasTheme {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for CanvasTheme {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_name(value).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_names_match_css_and_ipc() {
        assert_eq!(CanvasTheme::ALL.len(), 5);
        assert_eq!(
            serde_json::to_string(&CanvasTheme::Minimalist).unwrap(),
            "\"minimalist\""
        );
        assert_eq!(
            serde_json::to_string(&CanvasTheme::MinimalistDark).unwrap(),
            "\"minimalist-dark\""
        );
        assert_eq!(
            serde_json::to_string(&CanvasTheme::OxidizedIron).unwrap(),
            "\"oxidized-iron\""
        );
        assert_eq!(
            serde_json::to_string(&CanvasTheme::Nord).unwrap(),
            "\"nord\""
        );
        assert_eq!(
            CanvasTheme::from_name("minimalist_dark"),
            Some(CanvasTheme::MinimalistDark)
        );
        assert_eq!(CanvasTheme::from_name("oxidized_iron"), Some(CanvasTheme::OxidizedIron));
        assert_eq!(CanvasTheme::from_name("nord"), Some(CanvasTheme::Nord));
        assert_eq!(CanvasTheme::from_name("unknown"), None);
    }
}
