# Padrão

Um desktop Wayland do zero, em C (compositor + host) e JS/CSS/HTML (em `web/`).

Em vez de uma barra de tarefas, um **deck trapezoidal** que cresce ao passar o
mouse. Em vez de um wallpaper, um **canvas infinito** onde todas as janelas
abertas aparecem como fichas, e onde dá para escrever notas, colocar imagens e
vídeos.

> **Implementação Rust:** o workspace Cargo em `common/`, `compositor/src/main.rs`
> e `frontend/` reimplementa o deck em Rust/Smithay + WebAssembly. A
> implementação C original continua no repositório e pode ser usada em paralelo.

## Implementação Rust (workspace Cargo)

| Peça | Papel |
| --- | --- |
| `common/` | Contrato IPC compartilhado (`IpcMessage`, `DesktopApp`, `ClusterPlacement`) e `CanvasTheme` com `serde`. |
| `compositor/` | Compositor Wayland com **Smithay 0.7** e backend aninhado **winit**; `Space<Window>` para as janelas `xdg_toplevel`; servidor Tokio com socket Unix, WebSocket e HTTP estático; bordas dos temas Minimalistas em `minimalist_render.rs`. |
| `frontend/` | Frontend **wasm-bindgen + web-sys**: canvas HTML5, pan/zoom, temas, glow da janela ativa, IPC WebSocket, camada SVG de desenho/imagens e deck trapezoidal de ferramentas/aplicativos; CSS e grid de papel milimetrado em `minimalist_theme.rs` (claro) e `minimalist_dark_theme.rs` (negro). |
| `shell-rust/` | Cliente C **GTK3 + WebKitGTK** (`spatial-deck-shell`) que hospeda a página dentro do compositor. É uma janela sem decoração; o compositor a reconhece pelo `app_id` `spatial-deck-shell`, fixa em `(0,0)` no tamanho da saída, desenha abaixo das janelas dos apps e a exclui do pan/zoom e do posicionamento do canvas. |

### Build

```sh
cargo build --workspace
wasm-pack build frontend --target web --out-dir ../web/pkg
```

Se `wasm-pack` não estiver instalado, `./build-wasm.sh` tenta o fallback
`cargo build --target wasm32-unknown-unknown` + `wasm-bindgen-cli`.

### Rodar

```sh
./run-rust.sh
```

O script compila o workspace, sobe o compositor e abre o `spatial-deck-shell`
(GTK/WebKit) em `WAYLAND_DISPLAY=spatial-deck`, carregando a UI em
`http://127.0.0.1:7338/?theme=…`. Sem o shell o compositor mostra apenas o
fundo sólido do tema — é ele que pinta o canvas, o deck e as fichas. Ao sair
(`Ctrl-C`), os dois processos são encerrados e o socket é limpo.

A janela do compositor é trazida para a frente automaticamente (`wmctrl -a`,
quando disponível) e marcada como `above` (`wmctrl -r 'Spatial Deck' -b
add,above`). Sem isso o gerenciador de janelas pode abri-la atrás do terminal,
e os cliques do mouse vão para a janela de cima em vez do desktop.

Variáveis úteis:

| Variável | Padrão | Uso |
| --- | --- | --- |
| `SPATIAL_DECK_THEME` | `default` | Tema inicial passado na URL (`default`, `minimalist`, `minimalist-dark`, `oxidized-iron`). |
| `SPATIAL_DECK_URL` | `http://127.0.0.1:7338/?theme=…` | URL carregada pelo shell; sobrepõe o tema da URL. |
| `SPATIAL_DECK_SOFTWARE` | desligado | `1` desativa a aceleração de hardware do WebKit no shell (fallback para drivers GL problemáticos). |
| `RUST_LOG` | `info` | Nível de log do compositor (log em `/tmp/spatial-deck.log`). |

O compositor abre uma janela aninhada `Spatial Deck` na sessão gráfica atual e
expõe:

| Endpoint | Uso |
| --- | --- |
| `WAYLAND_DISPLAY=spatial-deck` | Socket Wayland para clientes (`foot`, `kitty`, `gtk3-demo`, …). |
| `/tmp/spatial-deck.sock` | IPC nativo, uma mensagem JSON por linha. |
| `ws://127.0.0.1:7337` | Ponte WebSocket usada pelo frontend WASM. |
| `http://127.0.0.1:7338/` | Página `web/rust/index.html` + `web/pkg/`. |

Exemplo de cliente:

```sh
WAYLAND_DISPLAY=spatial-deck foot
```

O frontend pode ser aberto no navegador da sessão normal em
`http://127.0.0.1:7338/`. Temas também podem ser testados pela URL:
`?theme=default`, `?theme=minimalist`, `?theme=minimalist-dark` ou
`?theme=oxidized-iron`.

### Protocolos Wayland suportados

| Protocolo | Uso |
| --- | --- |
| `xdg-shell` + `xdg-decoration` | janelas dos apps, com barra de título do lado do servidor |
| `wlr-layer-shell` | barras/overlays ancorados nas bordas, fora do pan/zoom; `top`/`overlay` recebem ponteiro e layers `exclusive` recebem teclado |
| `ext-foreign-toplevel-list` | lista de janelas (título e `app_id`) para barras e taskbars externas |
| `xdg-activation` | foco ao ativar uma janela; `LaunchApp` injeta `XDG_ACTIVATION_TOKEN` no processo iniciado |
| `xwayland-shell` | janelas X11 pelo XWayland próprio |
| `wl_shm` / `wl_seat` / `wl_output` | buffers de software, teclado/ponteiro e saída |

Teste rápido do layer-shell, com o desktop rodando:

```sh
# retângulo sólido 260x120 ancorado em top|left (camada top)
WAYLAND_DISPLAY=spatial-deck cargo run -p compositor --example layer_test

# lista as janelas expostas por ext-foreign-toplevel-list
WAYLAND_DISPLAY=spatial-deck cargo run -p compositor --example layer_test -- --toplevels
```

O exemplo fica em `compositor/examples/layer_test.rs` e serve de cliente mínimo
para depurar os dois protocolos sem depender de uma barra completa.

### Serviços D-Bus (`shell-services`)

O crate `shell-services` (binário `spatial-deck-services`) é o cérebro D-Bus do
shell. Roda ao lado do compositor, conectado ao socket Unix; o compositor
repassa as mensagens entre as conexões Unix e WebSocket sem eco para quem
enviou. Sobe junto com `./run-rust.sh` e não derruba o desktop se cair.

Primeira função: daemon `org.freedesktop.Notifications`, com histórico, DND e
expiração.

| Item | Detalhe |
| --- | --- |
| Binário | `target/debug/spatial-deck-services` |
| Log | `/tmp/spatial-deck-services.log` |
| Nome D-Bus | `org.freedesktop.Notifications`; se estiver ocupado pelo daemon da sessão, cai para `org.freedesktop.Notifications.SpatialDeck` |
| Substituir o daemon da sessão | `SPATIAL_DECK_NOTIFY_REPLACE=1 ./run-rust.sh` |
| Histórico | `~/.config/spatial-deck/notifications.json` (últimas 100) |
| Capacidades | `actions`, `body`, `body-markup`, `icon-static`, `persistence` |

Teste sem roubar o nome do daemon da sessão:

```sh
gdbus call --session \
  --dest org.freedesktop.Notifications.SpatialDeck \
  --object-path /org/freedesktop/Notifications \
  --method org.freedesktop.Notifications.Notify \
  "Teste" 0 "dialog-information" "Resumo" "Corpo" "[]" "{}" 3000
```

Com o nome padrão livre (`dbus-run-session`, por exemplo), `notify-send` e
qualquer aplicativo funcionam direto. Mensagens IPC: `notification`,
`notification_closed`, `notification_action`, `notification_hold`, `dnd_state`,
`notification_history` e `notification_history_request`.

### Notificações na interface

- **Popups** no canto superior direito: empilham, mostram app, título, corpo,
  hora e botões de ação. Clique no corpo dispara a ação `default`; o `×`
  dispensa. O prazo segue `expire_timeout` (`0` nunca, `-1` = 5 s, críticas
  ficam); passar o mouse **pausa** o prazo no shell e no daemon
  (`notification_hold`).
- **Críticas** (`urgency = 2`) têm borda vermelha e furam o DND.
- **DND** no HUD: silencia popups de urgência normal/baixa (o histórico
  continua registrando). O estado vem do `shell-services`, então sobrevive a
  recarregar a página.
- **Histórico** ("Avisos" no HUD): lista com app, título, corpo, hora e ações;
  "Limpar" esvazia só a lista local. Com o painel aberto, novas notificações
  não geram popup (ficariam atrás dele).
- Corpo com markup (`<b>`, `<i>`, links) é convertido em texto simples e
  escapado — não é renderizado como HTML.
- Popups e painel entram em `ChromeRects`, então ficam acima das janelas reais
  e recebem cliques; os vãos entre popups continuam clicáveis no canvas.

### Temas do frontend Rust

| Tema | Nome | Visual |
| --- | --- | --- |
| Azul Padrão | `default` | azul profundo, aciano/teal, grade azul |
| Minimalista | `minimalist` | papel claro `#f7f7f2`, texto/contornos pretos, grade milimetrada |
| Minimalista Negro | `minimalist-dark` | fundo `#000000`, texto/contornos brancos, grade milimetrada branca |
| Ferro Oxidado | `oxidized-iron` | ferrugem, verdete e metal, grade quente |

### Desenho, imagens e deck de aplicativos

O deck trapezoidal fica no centro da borda inferior e reúne as ferramentas do
canvas e a grade de aplicativos. A camada de desenho é um SVG (`#draw-layer`)
que compartilha o mesmo `translate/scale` do mundo (`#draw-world`), então
traços e imagens acompanham o pan/zoom e o compositor sem perder a posição de
mundo.

| Ferramenta | Atalho | Comportamento |
| --- | --- | --- |
| Selecionar/mover | `V` | seleciona uma forma ou imagem; arraste para mover. `Shift` adiciona/remove da seleção. |
| Navegar | `H` | arrasta o canvas; a roda do mouse navega e `Ctrl` + roda dá zoom. |
| Caneta | `P` | traço livre com cor e espessura escolhidas no deck. |
| Linha | `L` | reta do ponto inicial ao final. |
| Retângulo | `R` | retângulo com prévia durante o arrasto. |
| Elipse | `O` | elipse com prévia durante o arrasto. |
| Texto | `T` | clique no canvas, digite no editor flutuante; `Enter` confirma, `Esc` cancela. |
| Borracha | `E` | clique numa forma ou imagem para removê-la. |
| Imagem | `I` | clique num ponto livre do canvas e escolha um arquivo; a imagem entra como `<image>` SVG com data URL. |

O deck também tem cor do traço, espessura (`1`–`24`), desfazer (`Ctrl+Z`),
refazer (`Ctrl+Shift+Z`), alternar a grade (`G`) e limpar o canvas. O histórico
guarda snapshots do SVG e é limitado para não crescer sem controle.

### Gestos do mouse

| Gesto | Comportamento |
| --- | --- |
| Roda do mouse | pan do canvas nos dois eixos. |
| `Ctrl` (ou `Meta`) + roda | zoom no ponto do cursor: girar para cima aproxima, para baixo afasta. |
| Botão do meio pressionado + arrastar | pan temporário, igual à ferramenta mão, sem trocar a ferramenta de desenho ativa. |
| Botão direito | pertence ao desktop: no canvas não abre menu algum e na visão geral fecha a janela apontada. |

O pan do botão do meio é capturado em fase de captura no `#viewport`, então
funciona sobre cartões, formas e imagens, e o `auxclick` nativo do navegador é
bloqueado. A roda usa um listener não passivo: com `Ctrl` o WebKit não aplica o
zoom de página, só o zoom do canvas.

O menu de contexto do WebKit (`voltar`/`recarregar`/`inspecionar elemento`) é
suprimido em duas camadas: o shell GTK devolve `TRUE` no sinal `context-menu` da
`WebKitWebView` (`shell-rust/main.c`) e o frontend cancela o evento DOM
`contextmenu`. Sem isso o popup nativo do WebKit abre um grab de ponteiro no
shell e o desktop aninhado inteiro para de responder a cliques e teclas.

O botão de grade no fim do deck abre o launcher com todos os `.desktop`
encontrados pelo compositor (a mesma lista de `AppList`). O botão mostra só o
ícone e o contador; o tooltip diz "Todos os aplicativos". A gaveta cresce para
cima, mantém a base colada na borda inferior, ocupa toda a largura da tela e
metade da altura.

O launcher tem:

- **busca tolerante**: nome exato, prefixo, trecho e subsequência (ex.: `frfx`
  encontra Firefox), com prioridade também para o comando;
- **favoritos**: a estrela em cada tile fixa/remove o comando; a seção
  "Favoritos" fica no topo e a escolha é salva em `localStorage`;
- **recentes**: os últimos 10 aplicativos abertos aparecem logo abaixo;
- **ícones reais**: o compositor serve `/icon/<nome>` a partir dos temas do
  freedesktop e o frontend cai para a inicial do nome quando não encontra;
- **teclado**: `↓`/`↑` movem o foco para o primeiro/último tile, as setas
  navegam entre tiles, `Enter` abre, `Esc` fecha.

Clicar num aplicativo envia `LaunchApp` ao compositor, que o inicia com
`WAYLAND_DISPLAY=spatial-deck`, `DISPLAY=:N` (XWayland) e as variáveis do
backend Wayland.

### Mover janelas e cartões

Há dois caminhos, ambos atualizando a janela real:

| Gesto | Quem inicia | Resultado |
| --- | --- | --- |
| Arrastar a barra de título do cliente (CSD) | cliente, via `xdg_toplevel.move` (`move_request`) | O compositor move a janela e transmite as posições de volta. |
| `Ctrl`, `Alt` ou `Super` + arrastar o botão esquerdo sobre a janela | compositor | Mesmo movimento, útil para clientes sem decoração (SSD). Ao rodar aninhado, use `Ctrl`: o WM da sessão hospedeira costuma capturar `Alt`/`Super` e mover a janela do deck inteira. |
| Botão **Redimensionar** do deck + arrastar sobre a janela | compositor | Redimensiona pelo canto mais próximo do clique, inclusive clientes sem borda acessível. Qualquer ferramenta de desenho desliga o modo. |

O compositor anuncia `xdg-decoration` em modo **servidor**: a barra de título é
o cartão estilizado do shell, desenhado 39px acima do conteúdo. Clientes que
respeitam o protocolo, como Alacritty, deixam de desenhar a barra própria;
clientes que o ignoram continuam com a decoração deles (pode haver duas barras
nesse caso, sem quebra de função). O estado `Activated` é enviado à janela
focada para que a decoração do cliente também acompanhe o foco.

A área de clique de uma janela usa a **bbox da árvore de superfícies**, não só o
retângulo do conteúdo: a barra CSD fica acima/à esquerda da origem de mundo e
antes não recebia cliques. O recorte visual na linha do HUD também recusa
cliques na faixa invisível.

A faixa de 8px no topo da janela **não** inicia redimensionamento no
compositor: ela pertence à barra de título. Bordas esquerda, direita e inferior
continuam redimensionando pelo compositor, e clientes CSD também podem pedir
resize pelo próprio `xdg_toplevel.resize`.

O arrasto usa o mesmo `translate/scale` do mundo, então funciona com zoom
aplicado. O compositor guarda a posição em coordenadas de mundo
(`ManagedWindow::world`), aplica a transformação do canvas no `Space` e envia
`ClusterUpdate` aos clientes WebSocket apenas durante `update_move`/`end_move`,
sem ecoar o `ClusterUpdate` vindo do frontend (evita laço). Janelas maximizadas
pelo `AutoMaximize` ficam ancoradas em `(0,0)` e não podem ser arrastadas até
serem restauradas.

Pedidos de maximizar vindos do cliente são ignorados e o atalho de maximizar
apenas restaura: maximização própria ainda não faz parte do desktop (a barra de
título sairia da tela).

### XWayland

O compositor sobe um XWayland próprio (`SPATIAL_DECK_XWAYLAND=0` desativa) e
passa `DISPLAY=:N` aos aplicativos iniciados pelo deck. Janelas X11 são
adotadas no canvas como as Wayland: recebem cartão/barra estilizada, foco,
movimento, redimensionamento e fechamento. A posição é sincronizada no root do
XWayland para menus e diálogos.

Limitações atuais: janelas override-redirect (menus e tooltips X11) ainda não
são rastreadas como popups do canvas; maximizar/tela cheia de clientes X11 está
desativado junto com os demais. O XWayland não impede o desktop de subir: se o
binário faltar ou falhar, só os clientes Wayland continuam.

### Zoom real e visão geral

O zoom do canvas não é só um pan: as superfícies reais são escaladas no
renderizador (`compositor/src/canvas_render.rs`). O `Space` do Smithay posiciona
as janelas, mas deriva o tamanho de cada elemento do buffer do cliente; o
`ScaledSurfaceElement` sobrescreve `geometry()` com o fator do canvas e escala
também o deslocamento dos popups (menus continuam ancorados ao campo).

| Ação | Onde | Atalho / controle |
| --- | --- | --- |
| Aumentar/diminuir zoom | HUD, roda do mouse com `Ctrl`, compositor | `+` / `−`, `Ctrl+=` / `Ctrl+-` |
| Voltar a 100% | HUD | `1:1`, `Ctrl+0` |
| Enquadrar todas as janelas | HUD | `Ajustar`, `Ctrl+Shift+F` |
| Visão geral | HUD, compositor | botão **Visão geral**, `Ctrl+Alt+O` ou `Ctrl+Shift+O` |
| Sair da visão geral | teclado, clique | `Esc` ou clique fora das miniaturas |

Na visão geral cada janela vira uma miniatura ao vivo numa grade calculada em
coordenadas de tela; o mundo e os tamanhos das janelas não mudam. Clique
esquerdo escolhe e foca a janela, clique direito fecha, e qualquer
`CanvasTransform` ou atalho de zoom sai do modo. O HUD e o deck continuam
visíveis e utilizáveis por cima da grade.

Sem nenhuma janela Wayland real, a grade é formada pelos próprios cartões de
demonstração do shell: o compositor avisa `overview_state` com `windows: 0` e o
frontend os reposiciona na mesma área útil. Assim o botão **Visão geral** nunca
fica inerte numa área de trabalho vazia.

### Contrato IPC

O socket Unix e o WebSocket compartilham um barramento: mensagens de um
cliente são repassadas às demais conexões (sem eco para o remetente) e o
compositor publica as suas. É assim que o `shell-services` entrega
notificações ao frontend e recebe de volta DND, ações e fechamentos.

O JSON é internamente marcado por `type` (snake_case):

```json
{"type":"canvas_transform","scale":1.5,"offset_x":120.0,"offset_y":-40.0}
{"type":"cluster_update","placements":[{"id":"1","x":90.0,"y":90.0,"w":900.0,"h":600.0,"title":"Alacritty"}]}
{"type":"chrome_rects","rects":[{"x":32.0,"y":14.0,"w":1216.0,"h":46.0}],"hud_bottom":60.0}
{"type":"auto_maximize","window_id":1,"state":true}
{"type":"set_theme","theme":"oxidized-iron"}
{"type":"app_list","apps":[{"name":"foot","exec":"foot","icon":"utilities-terminal"}]}
{"type":"launch_app","exec":"foot"}
{"type":"zoom_canvas","action":"fit"}
{"type":"toggle_overview"}
{"type":"overview_state","active":true,"windows":2}
{"type":"toast","message":"Nenhuma janela real aberta"}
```

- `CanvasTransform` aplica `screen_x = world_x * scale + offset_x` e
  `screen_y = world_y * scale + offset_y` no `Space`.
- `ClusterUpdate` atualiza a posição de mundo das janelas pelo id.
- `AppList` é enviada pelo compositor assim que um cliente Unix ou WebSocket
  conecta. A varredura usa `freedesktop_entry_parser` em
  `/usr/share/applications`, `~/.local/share/applications`, `XDG_DATA_HOME` e
  `XDG_DATA_DIRS`.
- `LaunchApp` é enviado pelo deck quando um tile da gaveta é clicado. O
  compositor resolve o comando `Exec` e inicia o processo no ambiente Wayland
  aninhado.
- `SetTheme` troca o atributo `data-canvas-theme` no `<html>`, atualiza
  `#active-window-glow` e envia o `CanvasTheme` ao compositor. No tema
  `minimalist`, o frontend injeta as regras monocromáticas e desenha o grid de
  papel milimetrado claro; o compositor usa fundo `#f7f7f2` e contornos pretos
  sólidos (`#000000`) nas janelas. No tema `minimalist-dark`, a mesma grade é
  desenhada em branco sobre fundo `#000000`; o compositor usa fundo negro e
  contornos brancos sólidos (`#ffffff`).
- `ZoomCanvas` é enviado pelo compositor quando um atalho de zoom é usado; o
  frontend aplica `in`, `out`, `reset` ou `fit` em torno do centro da área
  visível.
- `ToggleOverview` alterna a visão geral. O compositor responde com
  `OverviewState` para o HUD marcar o botão como pressionado.

### AutoMaximize

O frontend calcula, para cada cartão de janela, o tamanho em tela
(`largura * scale`, `altura * scale`). Quando qualquer dimensão atinge **90%**
do viewport, ele envia `AutoMaximize { state: true }` para o id correspondente;
ao voltar abaixo do limiar, envia `state: false`. O compositor responde com
`xdg_toplevel::State::Maximized` e um `configure` do tamanho da saída.

### Observações

- O backend é aninhado: não precisa de TTY, root nem DRM.
- O compositor mantém `wl_seat`, teclado e ponteiro básicos; clientes que
  dependem de protocolos opcionais (layer-shell, screencopy, text-input)
  continuam avisando no stderr.
- `SPATIAL_DECK_WEB_ROOT` pode apontar para outro diretório estático.
- `cargo test -p common` cobre o round-trip das mensagens IPC e os nomes
  serializados dos temas; `cargo test -p compositor` cobre a geometria das
  bordas compartilhadas pelos temas Minimalista e Minimalista Negro.

## Peças (implementação C original)

| Parte | O que é |
| --- | --- |
| `compositor/` | Compositor Wayland em C sobre **wlroots 0.20**. Aninhado na sessão atual (janela `WL-1`), sem precisar de TTY nem root. |
| `shell-host/` | Processo GTK3 + gtk-layer-shell + WebKitGTK. Cria as layer surfaces (canvas de fundo, deck de topo) e hospeda a UI. |
| `tools/padrao-shot.c` | Cliente Wayland que fala `wlr-screencopy-v1` e grava PNG da saída (ferramenta de teste; alternativa autossuficiente ao `grim`). |
| `web/` | **Toda a interface web** (fonte única): `index.html`+`app.js`+`style.css` (a página que o compositor mostra como fundo), `canvas/`, `deck/` e `shared/` (o shell). |
| `deps/` | `fetch-deps.sh` baixa os RPMs de desenvolvimento sem sudo; `env.sh` aponta `pkg-config`/headers para o sysroot extraído. |

## Rodar

```sh
cd padrao
make            # compila compositor + host
./run.sh        # sobe tudo aninhado na sessão Wayland atual
```

`run.sh` sobe o compositor, espera o socket de IPC, sobe o host apontando
`WAYLAND_DISPLAY=padrao`, e vigia os dois (se um cai, derruba o outro e limpa
o socket). `Ctrl-C` encerra.

Para capturar a tela de teste:

```sh
make tools/padrao-shot
WAYLAND_DISPLAY=padrao ./tools/padrao-shot saida.png
```

E para capturar os **estados do deck** (o crescido e a grade de aplicativos)
de uma vez, com o desktop já rodando:

```sh
./run.sh &          # o compositor precisa estar no ar
tools/capturar.sh   # a porta do host é lida do log
```

O script usa o gancho `/debug/emit` (abaixo) para forçar cada estado e
grava em `docs/screenshots/`.

### Gancho de depuração

O host expõe uma rota só em loopback que injeta um evento `Padrao.emit()` numa
página, útil para forçar estados do deck sem depender de mouse/teclado reais:

```sh
# sobe o estado "open" (hover) do deck
curl "http://127.0.0.1:<porta>/debug/emit?page=deck&json=%7B%22t%22%3A%22dbg%22%2C%22state%22%3A%22open%22%7D"
```

O `json` é um objeto JSON URL-encoded e precisa do campo `"t"`, porque
`Padrao.emit()` despacha pelo tipo do evento (o deck registra `t:"dbg"`).
Sem `"t"` o evento é entregue a nenhum handler e nada acontece — silenciosamente.
Aceitos: `{"t":"dbg","state":"open"}`, `{"t":"dbg","state":"grid"}` e
`{"t":"dbg","state":"rest"}`. A porta do host aparece no stderr dele ao subir.

O mesmo caminho serve para o canvas (`page=canvas`): injete o evento que a
página escuta, por exemplo `{"t":"view","x":0,"y":0,"z":1}`.

## Temas do canvas

O app de `web/rust/` (o fundo do compositor) tem cinco temas, trocáveis pelos
botões `.theme-chip` no HUD:

| Tema | Seletor | Visual |
| --- | --- | --- |
| Azul Padrão | `default` | o original: azul profundo, aciano/teal, grade azul |
| Minimalista | `minimalist` | só tons de cinza, sem brilho, grade quase invisível |
| Minimalista Negro | `minimalist-dark` | fundo preto com traços claros, grade discreta |
| Ferro Oxidado | `oxidized-iron` | ferrugem, verdete e metal, grade quente |
| Nord | `nord` | paleta ártica (`nord0`–`nord15`), fundo azul-acinzentado |

No CSS cada tema é um bloco `[data-canvas-theme="…"]` que define as
variáveis de cor, bordas, fundos da grade e os estilos da moldura
`#active-window-glow` (o contorno do item em foco). O seletor é escrito no
`<html>`.

O frontend guarda a escolha no `localStorage` (`spatial-deck.theme`) e marca o
botão ativo. A URL tem precedência sobre a escolha salva:

```
web/rust/index.html?theme=nord
```

É o que o compositor usa para abrir o fundo já num tema:

```sh
SPATIAL_DECK_THEME=nord ./run-rust.sh
```

## Camada web (fundo em WebKitGTK)

O compositor cria **em C** (`compositor/src/main.c`) uma layer surface com
WebKitGTK: uma janela GTK conecta no socket do *próprio* Padrão
(`WAYLAND_DISPLAY=padrao`) e o `gtk-layer-shell` a transforma numa camada do
compositor, mostrando `web/index.html` como fundo da tela — sem processo
extra.

A camada roda numa thread própria porque o GDK faz um *roundtrip* para buscar
os globals do servidor, e esse servidor é o próprio compositor: só responde
dentro do event loop dele, que fica na thread principal.

Por isso o compositor **não** usa `wl_display_run()`: usa um loop manual
(`wl_display_flush_clients` + `wl_event_loop_dispatch`) que continua rodando
durante o encerramento, para que o cliente GTK se despida com elegância. Sem
isso, o teardown do GTK/GL corre em paralelo com a limpeza do display e o
heap do processo acaba corrompido.

Variáveis de ambiente:

| Variável | Padrão | Efeito |
| --- | --- | --- |
| `PADRAO_WEB_FILE` | `web/index.html` | página local a abrir |
| `PADRAO_WEB_LAYER` | `bottom` | camada: `background`, `bottom`, `top` ou `overlay` |
| `PADRAO_WEB_GPU` | (desligado) | `1` reativa o OpenGL do WebKit (por padrão a camada renderiza por software: o driver GL do Mesa neste ambiente corrompe o heap) |

> O `bottom` é o padrão de propósito: o canvas do shell (`web/canvas`) é
> opaco e, na camada `background`, cobriria a página web.

## Como as peças conversam

```
   canvas.js / deck.js  (WebKit)
        |  webkit.messageHandlers.p
   shell-host  (GTK)
        |  Unix socket  $XDG_RUNTIME_DIR/padrao.sock   JSON por linha
   compositor  (wlroots)
```

O host é o único cliente do socket. Ele espelha os eventos do compositor para
as páginas JS via `Padrao.emit(<json>)`, e reúne os comandos vindos da UI
(mover câmera, focar/fechar janela, redimensionar o deck) para mandar de volta.

Contrato das mensagens está descrito em `shell-host/src/padrao_host.h`.

### Transform do canvas e cluster (tiling)

O canvas manda ao compositor a transformação em uso, e o compositor
reposiciona cada janela para que ela acompanhe o zoom exatamente:

```jsonc
// canvas -> host -> compositor
{ "t": "canvas_transform",
  "scale": 1.4,              // zoom
  "offset_x": -185, "offset_y": 227 }   // origem do mundo, em px de tela

{ "t": "cluster_update",
  "placements": [ { "id": 7, "x": -260, "y": -160 } ] }   // tiling em lote
```

Com isso, a posição de cada janela na tela é

```
screen_x = (window_x * scale) + offset_x
screen_y = (window_y * scale) + offset_y
```

A escala visual é a da **saída** (o zoom do canvas), então as janelas
crescem junto com o canvas.

### Lista de aplicativos

O shell-host varre **todos** os `.desktop` de `/usr/share/applications`,
`/usr/local/share/applications` e `~/.local/share/applications`, extrai
`Name`, `Exec` e `Icon`, ordena alfabeticamente e descarta apenas o que tem
`NoDisplay=true`. A lista completa vai para a UI:

```jsonc
{ "t": "app_list", "apps": [ { "id": "firefox", "name": "Firefox",
  "icon": "firefox", "exec": "firefox %u" } ] }
```

O deck consome esse evento e desenha a grade dinamicamente.

## Por que aninhado

O compositor roda como uma janela normal dentro da sessão KDE/GNOME atual, o
que permite desenvolver e depurar sem matar a sessão de trabalho nem escrever
em `/dev`. Em TTY viria o modo direto, mas o aninhamento é o caminho de
desenvolvimento.

## Layout do código

```
compositor/src/main.c    bootstrap wlroots, registra todos os listeners
compositor/src/output.c  cria saídas, comite de frame, zoom (escala de saída)
compositor/src/view.c    janelas xdg-shell (foco, mover, fechar)
compositor/src/input.c   cursor, teclado, roda (zoom)
compositor/src/layer.c   layer surfaces (canvas e deck)
compositor/src/ipc.c     socket Unix, protocolo JSON
shell-host/src/main.c    ponte JS<->compositor, estado, apps, ícones
shell-host/src/http.c    servidor local que serve a UI, mídia e ícones
shell-host/src/ipc.c     cliente do socket, reconexão
shell-host/src/jx.c      JSON mínimo sem dependências
```

## Notas de build

- As dependências ficam em `~/.local/share/padrao/{rpms,sysroot}`; nada em
  `/usr` e nada com sudo.
- Os headers de protocolo (`wlr-*-protocol.h`) são gerados por
  `wayland-scanner` porque os RPMs de dev do wlroots não os instalam.
- Os caminhos no `Makefile` são relativos de propósito: o diretório do projeto
  contém espaços, e o make não lida bem com espaços em `$(wildcard)`.
- Há também um `meson.build` equivalente (meson + ninja), que convive com o
  `Makefile`:

  ```sh
  source deps/env.sh     # obrigatório: aponta pkg-config para o sysroot
  meson setup build
  ninja -C build
  ```

  Os binários saem em `build/` (`padrao-compositor`, `padrao-host`,
  `padrao-shot`). O `run.sh` usa os de `build/` quando existem e cai para os
  do `make` caso contrário.
