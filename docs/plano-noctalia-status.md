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

## Etapa 4 — Barra modular, relógio/calendário e regiões — CONCLUÍDA

- HUD reorganizado em três grupos (esquerda/centro/direita) com módulos
  movidos por `append_child`; config em `localStorage` (`spatial-deck.bar`)
  com slot por módulo, ordem (↑/↓) e relógio 12/24h.
- Painel **Barra** com um `<select>` por módulo (Esquerda/Centro/Direita/
  Oculto) e checkbox de 24h; reaplica e persiste a cada mudança.
- Tema virou módulo compacto: botão "Tema: …" abre a lista dos cinco chips.
- Relógio no centro (hora + data em pt-BR), ticker de 1 s; clique abre o
  calendário mensal com navegação ‹ › e o dia de hoje destacado.
- Regiões: `CanvasRegion` e mensagens `RegionList`, `RegionListRequest`,
  `SaveRegion`, `RegionSaved`, `GotoRegion`, `DeleteRegion` em `common/` com
  testes; `RegionStore` no `shell-services` persiste
  `~/.config/spatial-deck/regions.json`.
- Chips na barra com destaque da região ativa (distância do transform),
  salvar via input inline, excluir pelo `×`, atalhos `Ctrl+1..9` e animação
  de 250 ms (smoothstep) que o compositor acompanha a cada frame.
- Popovers (calendário, temas, configuração) entram em `ChromeRects` e usam
  `--popover-bg` opaco por tema.

### Evidências de teste (17/11/2026)

- `cargo test --workspace` verde (20 + 20 + 16).
- Barra visível em 1920 e 1280; relógio central, calendário de Novembro/2026
  com o dia 17 destacado; painel Barra com todos os módulos; menu de temas.
- Duas regiões salvas por IPC e persistidas no JSON; chips apareceram com a
  ativa destacada.
- `gtk3-demo` (duas janelas) movidas por `cluster_update`: `goto_region` para
  a região B levou o zoom a 60% e reposicionou as janelas reais; voltar para
  a região A restaurou 100% e as posições.
- Reinício do desktop: chips recarregados do JSON (persistência) e região
  ativa recalculada.

### Limitações

- A interação com os `<select>`/setas da configuração da barra não foi
  automatizada (sem injeção de clique no WebKit); o painel foi verificado
  visualmente e o caminho persistir→reaplicar é direto.
- Salvar região usa nome digitado no input inline; renomear não existe (exclui
  e salva de novo).
- Regiões não são sincronizadas entre duas instâncias do frontend além do
  broadcast normal.

## Etapa 5 — Bandeja (SNI) e mídia (MPRIS) — CONCLUÍDA

- `shell-services` ganhou `tray.rs` e `media.rs`.
- Bandeja: watcher próprio quando o nome está livre; caso contrário host do
  watcher da sessão (`RegisterStatusNotifierHost` + leitura de
  `RegisteredStatusNotifierItems`), sem disputar o nome.
- Itens consultados a cada 2 s (título, ícone, status, menu, `ItemIsMenu`) com
  timeout de 900 ms por item — um item morto não trava a lista.
- Menu dbusmenu: `AboutToShow` + `GetLayout` com layout tipado recursivo
  (`(ia{sv}av)` → `MenuLayout`), renderizado com submenus e separadores;
  clique chama `Event(id, "clicked")`. `Activate`/`SecondaryActivate` chamam o
  item com timeout.
- MPRIS: descoberta por `ListNames`, player com `Playing` preferido;
  `Metadata` (título, artistas; fallback para o nome do arquivo em
  `xesam:url`), `PlaybackStatus`, `CanGoNext/Previous`; comandos
  `PlayPause`/`Next`/`Previous`.
- IPC: `tray_update`, `tray_activate`, `tray_secondary_activate`,
  `tray_request`, `tray_menu_request`, `tray_menu`, `tray_menu_event`,
  `media_state`, `media_command`, `media_request` + testes.
- Frontend: módulos Bandeja e Mídia (novos na config da barra), ícones com
  fallback pela inicial, menu popover, botões de mídia e esmaecimento quando
  o player não suporta anterior/próximo.

### Evidências de teste (17/11/2026)

- `cargo test --workspace` verde (22 + 20 + 20).
- Bandeja: 3 itens reais do barramento apareceram na barra; menu do
  `xwaylandvideobridge` (`Reset Bridge`, `Quit`) renderizado via
  `tray_menu_request`; `tray_activate` em "Easy Effects" sem erro.
- Mídia: `cvlc --intf dummy --no-video` → módulo mostrou `my_video-1` e botão
  de pausa; `media_command play_pause` pausou o VLC e o botão virou play;
  ao fechar o VLC o módulo desapareceu sozinho.
- Primeiro `tray_update` chegava antes do WebSocket: resolvido com
  `tray_request`/`media_request` no `onopen`.
- Item morto no barramento travava o polling: resolvido com timeout por item.

### Limitações

- `IconPixmap` não é decodificado (usa `IconName`/`/icon/<nome>` ou a inicial).
- Menu: lê `GetLayout` inteiro, mas só interpreta `label`, `enabled`,
  `visible` e `type=separator`; estados de toggle e ícones de item não são
  mostrados.
- Em sessão compartilhada a bandeja mostra os itens do host (não só os do
  deck); é o comportamento esperado de um host do watcher.
- `ItemIsMenu` abre o menu no clique esquerdo; não há tooltip próprio
  (`ToolTip` do SNI ignorado).

## Correção — travamento ao abrir imagem — CONCLUÍDA

Sintoma: o desktop travava ao escolher uma imagem na ferramenta `I`.

Causas prováveis atacadas:

1. **Memória/histórico**: cada `<image>` carregava o data URL completo e cada
   um dos 60 snapshots do histórico serializava o SVG inteiro, duplicando
   todos os base64. Uma foto de 10 MB virava ~13 MB × 60.
   - Agora imagens acima de 600 KB são redimensionadas (maior lado 1600 px) e
     recomprimidas em JPEG (qualidade 0.85, caindo até caber em 1,6 MB);
     arquivos acima de 40 MB e formatos inválidos são recusados com aviso.
   - O histórico guarda só `data-image-id`; o base64 fica em `STATE.images` e
     é reidratado no desfazer/refazer.
2. **Portal do host**: o seletor podia abrir no portal do KDE, atrás da janela
   do deck (always-on-top) e bloquear a página. `run-rust.sh` agora exporta
   `GTK_USE_PORTAL=0`, forçando o diálogo GTK dentro do deck.
3. **Diálogo confundido com o shell**: qualquer janela do processo do shell
   era tratada como "janela do shell" (`is_shell_surface` por `app_id`), então
   o seletor ficava sem foco próprio. Agora só a primeira janela é adotada;
   janelas seguintes com a janela do shell como pai viram diálogo
   (`Z_DIALOG`, sem `pending.size` forçado, foco de teclado).

Validação: `cargo test --workspace` verde (22 + 20 + 20); compressão
verificada no WebKit com uma imagem sintética 3000×2000 (data URL válido de
572 KB, dentro do teto). O seletor em si depende de clique do usuário para
abrir (WebKit exige gesto), então a validação visual do diálogo ficou manual.

## Ajuste — janela redimensionável e camadas — CONCLUÍDO

- Janela aninhada redimensionável (mínimo 720×480). O compositor reconfigura
  a saída no resize; shell, deck e canvas acompanham. Validado alternando
  fullscreen 1920×1080 ↔ janela 1280×720 (log "saída redimensionada").
- Pilha explícita de janelas (fundo→topo). `raise_window`/`lower_window` por
  IPC, botões ▴/▾ na barra de cada cartão; foco também sobe a janela. Z
  bases: comuns 10.., maximizado 210, diálogo 220, shell 0.
- Imagens com camada `above`/`below` (padrão below). Botão de camadas no deck
  alterna as imagens selecionadas; imagens "acima" entram em `ChromeRects`
  (um retângulo por imagem, somando `data-tx/ty` do arrasto) e são
  redesenhadas sobre as janelas, com HUD/deck ainda por cima. Atualização
  durante pan/zoom limitada a ~8 Hz.
- Histórico, desfazer/refazer e compressão de imagens continuam valendo.

Limitação: o recorte da imagem "acima" vem da superfície do shell, então
transparência mostra o canvas (não a janela atrás).

## Ajuste — redimensionar imagens e painel de camadas — CONCLUÍDO

- **Alças de redimensionamento** nas imagens selecionadas (quatro cantos,
  proporção preservada, mínimo de 40 px). O deslocamento de arrasto é
  incorporado antes do resize e o histórico/recorte chrome são atualizados.
- **Painel "Camadas"** no deck, listando janelas reais (título + ▴/▾ via
  `raise_window`/`lower_window`) e imagens (camada atual, alternar
  acima/abaixo, reordenar entre imagens e selecionar pelo nome).
- Imagens "acima" selecionadas têm o recorte do chrome expandido em 10 px para
  que as alças continuem visíveis e clicáveis sobre janelas.
- Validado visualmente com duas imagens sintéticas (uma `abaixo`, uma `acima`),
  alças visíveis e painel com as duas linhas; `cargo test --workspace` verde.

## Ajuste — nota rápida — CONCLUÍDO

- Botão de nota no deck cria um cartão no centro da vista (cascata de 26 px).
- Corpo em `<textarea>` estilizado como os cartões; arrasto pela barra de
  título (clique na textarea não inicia arrasto/pan) e `×` remove.
- Notas entram em `STATE.cards` (ids a partir de 10000, sem colidir com
  janelas/diálogos), mas não são enviadas como janelas reais.
- Persistência em `localStorage` (`spatial-deck.notes`): posição, tamanho e
  texto; salva ao digitar, ao arrastar e ao remover; carregadas no boot.
- Validado: nota criada, texto exibido, arrastada (estado) e recarregada após
  reiniciar o desktop sem o gancho de teste.

## Próxima etapa

Etapa 6 — rede, bateria, volume e brilho na barra. Nada iniciado.
