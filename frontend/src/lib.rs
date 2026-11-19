//! Frontend WebAssembly do Spatial Deck.
//!
//! No alvo nativo o crate apenas compila como uma biblioteca vazia para que
//! `cargo build --workspace` funcione sem precisar do alvo wasm32. Todo o
//! código de navegador vive em `web.rs` e é compilado somente para
//! `wasm32-unknown-unknown`.

#[cfg(target_arch = "wasm32")]
mod web;

/// Calculadora da busca do launcher (puro Rust, testável no host).
pub mod calc;

#[cfg(target_arch = "wasm32")]
pub mod minimalist_theme;

#[cfg(target_arch = "wasm32")]
pub mod minimalist_dark_theme;

#[cfg(target_arch = "wasm32")]
pub mod drawing;

#[cfg(target_arch = "wasm32")]
pub use web::*;

/// Stub nativo: mantém o workspace compilando em `cargo build --workspace`.
#[cfg(not(target_arch = "wasm32"))]
pub fn start() {}
