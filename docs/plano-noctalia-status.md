# Status do plano: shell completo (estilo Noctalia)

Referência: `/home/userx/.opencode/plan/spatial-deck-noctalia.md`
Atualizado ao final de cada etapa.

## Etapa 1 — Protocolos base no compositor — CONCLUÍDA

Entregue no compositor Rust (Smithay 0.7):

- **`wlr-layer-shell`** (`WlrLayerShellState` + `LayerMap` do Smithay)
  - superfícies ancoradas via `LayerMap::arrange`, configure inicial no
    primeiro commit, reposicionamento no resize da saída;
  - renderização por camada: `background`/`bottom` abaixo das janelas,
    `top`/`overlay` acima; nunca entram no pan/zoom do canvas;
  - ponteiro: hit-test em `overlay` e `top` antes das janelas;
  - teclado: layers `exclusive` recebem foco ao mapear; `on-demand` ao clique;
  - frame callbacks enviados para as layers.
- **`ext-foreign-toplevel-list`**: handle por janela (Wayland e X11) criado
  quando o id definitivo é atribuído; título/`app_id` atualizados em
  `title_changed`/`app_id_changed` (Wayland) e `property_notify` com
  `Title`/`Class` (X11); `closed` ao destruir. O shell (id 0) fica de fora.
- **`xdg-activation`**: `request_activation` foca a janela da superfície
  (tokens com mais de 10s são ignorados); `LaunchApp` cria token externo e
  injeta `XDG_ACTIVATION_TOKEN` no processo.

### Evidências de teste (17/11/2026)

- `cargo build --workspace` e `cargo test --workspace` verdes (15 + 20 testes).
- `wayland-info` no socket `spatial-deck`:
  `zwlr_layer_shell_v1` v4, `ext_foreign_toplevel_list_v1` v1,
  `xdg_activation_v1` v1.
- Cliente de teste `compositor/examples/layer_test.rs`:
  - imprime `CONFIGURED 260 120` e o retângulo azul aparece ancorado em
    top|left, abaixo da barra, acima do cartão do terminal (captura de tela);
  - `--toplevels` com `gtk3-demo` aberto no deck:
    `TOPLEVEL app_id="gtk3-demo" title="Application Class"` / `TOPLEVELS 1`.
- `LaunchApp` via socket Unix: processo recebeu
  `WAYLAND_DISPLAY=spatial-deck` e `XDG_ACTIVATION_TOKEN=...`.
- Log do compositor sem WARN/ERROR durante os testes.

### Limitações conhecidas / observações

- O chrome do shell (barra/HUD/deck) é desenhado por último e continua acima
  de layers `top`/`overlay`; quando a barra virar layer surface (etapa 4/7),
  o `ChromeRects` deve ser reduzido para não cobrir a barra.
- Popups de layer surface são renderizados, mas não recebem
  `xdg_popup.grab` próprio (menus de barras ainda não testados).
- Ativação xdg validada em nível de protocolo/global e no token do `LaunchApp`;
  o caminho `request_activation` → foco não foi exercitado por um cliente
  externo nesta etapa (código simples, coberto na etapa 5/7 quando a barra
  começar a ativar janelas).
- O exemplo de teste usa `wl_shm` e um arquivo temporário (sem `memfd`).
- Sem testes unitários novos nesta etapa: a geometria de ancoragem é
  responsabilidade do `LayerMap` do Smithay e foi validada com cliente real;
  não houve mensagem IPC nova para justificar round-trip serde.

## Próxima etapa

Etapa 2 — crate `shell-services` + daemon de notificações. Nada iniciado.
