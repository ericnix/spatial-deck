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

## Etapa 2 — `shell-services` + daemon de notificações — CONCLUÍDA

- Crate `shell-services` (binário `spatial-deck-services`, tokio + zbus 6.3),
  membro do workspace.
- Barramento IPC com origem: `(u64, IpcMessage)`. Conexões Unix e WebSocket
  recebem mensagens das outras sem eco; o compositor publica com origem 0.
  Notificações são apenas roteadas pelo compositor.
- Servidor D-Bus `org.freedesktop.Notifications` em
  `/org/freedesktop/Notifications`: `Notify` (actions, urgency, icon,
  expire_timeout, replaces_id), `CloseNotification`, `GetCapabilities`,
  `GetServerInformation`; sinais `NotificationClosed` e `ActionInvoked`.
- Nome padrão ocupado pelo daemon da sessão → fallback
  `org.freedesktop.Notifications.SpatialDeck`; `SPATIAL_DECK_NOTIFY_REPLACE=1`
  tenta substituir. O processo nunca derruba o desktop.
- Histórico (limite 100) e DND persistidos em
  `~/.config/spatial-deck/notifications.json`.
- Expiração num tick de 250 ms; motivos 1 (expirada), 2 (descartada pela UI)
  e 3 (`CloseNotification`).
- IPC novo em `common/`: `Notification`, `NotificationClosed`,
  `NotificationAction`, `DndState`, `NotificationHistory`,
  `NotificationHistoryRequest` + testes de round-trip.
- `run-rust.sh` sobe e encerra o serviço; log em
  `/tmp/spatial-deck-services.log`; queda do serviço não encerra o desktop.

### Evidências de teste (17/11/2026)

- `cargo test --workspace` verde; 11 testes no `shell-services`, incluindo
  D-Bus real (`Notify` → mensagem IPC; `CloseNotification` →
  `NotificationClosed`).
- Desktop rodando: `gdbus Notify` no nome alternativo → cliente Unix recebeu
  `{"type":"notification","id":1,...}`.
- Expiração de 800 ms → `notification_closed` com `reason: 1`.
- `NotificationHistoryRequest` → `notification_history` + `dnd_state`; DND
  ligado → `dnd_state.active = true`.
- `dbus-run-session` + `notify-send` → daemon assumiu o nome padrão e o socket
  recebeu `{"type":"notification","id":3,"app_name":"notify-send",...}`.
- Log do desktop sem WARN/ERROR; o shell loga apenas o aviso esperado de nome
  D-Bus ocupado.

### Limitações

- O frontend ainda não desenha notificações (etapa 3); mensagens
  desconhecidas são ignoradas com segurança (`from_json` falha → return).
- Corpo com markup é repassado cru; sanitização é da etapa 3.
- Ações: `ActionInvoked` é emitido, mas o frontend ainda não tem botões.
- Duas instâncias (ex.: teste em `dbus-run-session`) compartilham o mesmo
  arquivo de histórico; não há lock.

## Etapa 3 — UI de notificações no frontend — CONCLUÍDA

- Popups no canto superior direito (app, título, corpo, hora, ações), com
  `expire_timeout` respeitado e **hover pausando** o prazo no shell e no
  daemon (`notification_hold`).
- Clique no corpo dispara a ação `default`; botões disparam a ação
  correspondente e fecham o popup com motivo 2; `×` dispensa.
- Críticas (`urgency = 2`) com borda vermelha e furando o DND.
- DND no HUD, estado sincronizado por `dnd_state` (não é toggle: o valor é o
  resultado, então reentrega é idempotente).
- Histórico ("Avisos") com app/título/corpo/hora/ações, contador no HUD,
  "Limpar" local e fechamento por clique fora ou pelo botão `×`.
- Histórico recebido por `notification_history` é **mesclado por id**, não
  substituído: corrige a corrida em que um anúncio do daemon chegava depois
  de notificações mais novas e apagava o histórico do shell.
- Com o painel aberto, novas notificações não abrem popup (ficariam atrás).
- Popups e painel entram em `ChromeRects` (cada popup com seu retângulo).
- Tema: `--danger` adicionado aos 5 temas; o resto usa as variáveis já
  existentes.

### Evidências de teste (17/11/2026)

- `cargo test --workspace` verde (18 + 20 + 12 testes).
- `./build-wasm.sh` sem erros.
- Capturas com `notify-send`/`gdbus` no desktop real:
  - normal, crítica (borda vermelha), ação ("Abrir log"), markup (`<b>` virou
    texto), expiração automática;
  - DND ligado: botão destacado, popup normal suprimido e crítica exibida;
  - DND desligado: popup volta;
  - tema Minimalista: popups claros coerentes com o papel milimetrado;
  - `gtk3-demo` posicionado sob os popups via `cluster_update`: o chrome do
    shell aparece **acima da janela real** (ChromeRects funcionando).

### Limitações

- Histórico é local: "Limpar" não apaga o arquivo do daemon; um novo
  `notification_history` pode trazer itens antigos de volta.
- Não há ícones de app nos popups (usa a inicial do nome); a rota
  `/icon/<nome>` fica para a etapa 5.
- Corpo não renderiza HTML por segurança (markup vira texto).
- Acentos/emoji dependem das fontes do WebKitGTK.

## Próxima etapa

Etapa 4 — barra modular, relógio/calendário e regiões do canvas. Nada
iniciado.
