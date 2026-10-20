# Padrão

Um desktop Wayland do zero, em C (compositor + host) e JS/CSS/HTML (em `web/`).

Em vez de uma barra de tarefas, um **deck trapezoidal** que cresce ao passar o
mouse. Em vez de um wallpaper, um **canvas infinito** onde todas as janelas
abertas aparecem como fichas, e onde dá para escrever notas, colocar imagens e
vídeos.

## Peças

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
