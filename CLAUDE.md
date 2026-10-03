# nirlock — contexto para Claude

Desbloqueo facial por infrarrojo (estilo Windows Hello) para la pantalla de
bloqueo de Omarchy. Daemon Rust con sandbox + módulo PAM en C + plugin QML.
Público en https://github.com/lordyoyi/nirlock (MIT).

Leer primero: `README.md`, `DECISIONS.md`, `docs/DESIGN.md`, `docs/adr/`.
Este archivo es lo que **no** se deduce del código: las reglas, las trampas
que ya nos costaron horas, y el estado entre equipos.

## Equipos

Rodrigo retoma sesiones desde más de una máquina. **Este archivo es la memoria
compartida**: al cerrar una sesión con estado o decisiones nuevas, actualiza
"Estado" abajo y haz commit.

- **Zenbook UX3405CA** — máquina de referencia y **la única con cámara IR**
  (Shinetech `3277:0055`). Checkout en `~/Work/nirlock`. Aquí está instalado,
  se usa a diario, y de aquí sale todo lo medido en `docs/`.
- **notro** (Beelink, Arch + Omarchy, siempre encendido) — checkout en
  `~/Dev/nirlock`. **Sin cámara** y sin `onnxruntime-cpu`: sirve para compilar,
  tests, clippy y revisar scripts. Nunca para captura, enrolamiento ni
  desbloqueo.

## Reglas que no se negocian

1. **La contraseña siempre funciona en paralelo.** Ningún cambio puede dejar
   la pantalla sin campo de contraseña. Antes de tocar el plugin o PAM en el
   Zenbook, deja una consola de rescate abierta (Ctrl+Alt+F3);
   procedimiento en `docs/INSTALACION-Y-RESCATE.md`, que **también vive en
   Dropbox/01 - Proyectos** porque desde una TTY no se lee este chat.
2. El rostro es **solo** para la pantalla de bloqueo. Nunca LUKS, sudo,
   polkit ni login (`docs/POR-QUE-NO-EL-LOGIN.md`).
3. `nirlock-cam` **no escribe controles UVC ni sondea extension units, jamás**
   (ADR-0005: cámaras hermanas se han dañado así). La superficie de ioctls
   está enumerada y hay un test que la vigila.
4. El módulo PAM **nunca** devuelve `PAM_IGNORE` (está envenenado en tiempo de
   compilación); todo fallo termina en `pam_deny`. Los archivos de `pam.d/` se
   instalan byte a byte desde el repo, sin generar ninguno a mano.
5. Frames, plantillas y dumps de bench son **datos biométricos**: repo privado
   aparte, nunca aquí. En 47 commits nunca se subió uno; que siga así.
6. **No publicar un perfil de hardware que nadie verificó.** Un perfil que
   parsea pero no funciona se anuncia como soportado y manda a su dueño a
   depurar un archivo que generamos nosotros.

## Disciplina de trabajo (esto es lo que más nos ha costado)

**Verifica antes de afirmar, y verifica antes de arreglar.**

- Una revisión externa trajo 26 hallazgos y **la mayoría estaba obsoleta**: el
  árbol ya tenía los arreglos. Comprueba cada hallazgo contra el código actual
  antes de tocarlo. Vale para los míos también.
- Al revés: en la revisión previa a publicar, de 20 hallazgos sobrevivieron 15
  a una verificación adversarial. Los otros 5 eran plausibles y falsos.
- **No confundas "el archivo está bien en disco" con "el proceso vivo lo
  usa".** El instalador dejó el binario nuevo sin usar porque
  `systemctl enable --now` no le hace nada a un servicio ya corriendo. Mira
  `/proc/<pid>/cmdline` y la hora de arranque, no el `ls`.

**Documentación que miente es un bug, y aquí ya pasó dos veces.**

- T6 mitigaba el oráculo de puntajes con "ningún puntaje en journal"… y el
  daemon lo imprimía con tres decimales.
- T11 afirmaba que el envoltorio "solo se arma si el archivo PAM es
  byte-idéntico". Esa comprobación **nunca existió**. Corregido el 2026-10-03.
- Antes de citar una mitigación del modelo de amenazas como hecha, busca el
  código que la implementa.

**Lo que no está probado se dice, no se insinúa.** El README declara que el
test de foto impresa no se ha hecho y que el umbral sigue en validación. Eso
es deliberado y se mantiene.

## Trampas de este entorno

- **QML del plugin: `omarchy restart shell`.** Desactivar y reactivar el
  plugin re-instancia el **mismo código compilado** (caché de tipos de
  Quickshell). El log dice "Local plugin changed, reloading" igual, y eso es
  justo lo que lo hace parecer que funcionó. Para saber qué build corre, mete
  un marcador en un `console.log` y léelo con `journalctl --user`.
  `omarchy restart shell` se niega con la sesión bloqueada.
- **`sudo` no lleva el entorno del escritorio.** Cualquier llamada al shell de
  Omarchy desde un script con sudo necesita `HOME`, `XDG_RUNTIME_DIR` **y**
  `OMARCHY_PATH` (sin este último `omarchy-shell` sale con error; sin el
  segundo dice "no está corriendo" y sale 0). Ver `as_user()` en
  `scripts/nirlock-install`, que debe ser idéntico al de `nirlock-uninstall`.
- **No silencies el comando cuyo fallo importa.** `>/dev/null 2>&1` sobre el
  `plugin-enable` escondió dos bugs distintos durante dos instalaciones.
- **`cargo fmt --check` no está limpio en master** (12 archivos de código
  anterior). No reformatees todo dentro de un PR de otra cosa; si se hace,
  commit propio.

## Comandos

```
cargo build --release --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
make -C pam && make -C pam test
```

- `make -C pam test` es **hermético**: los lanes que genera apuntan a un
  socket inexistente. Antes hablaba con `/run/nirlock/sock`, el daemon real, y
  en el Zenbook hacía verificaciones faciales de verdad (20 en una corrida),
  con lo que tres tests que esperan "daemon inalcanzable" recibían un éxito
  auténtico. Un caso usa el lane empaquetado verbatim y **se salta** mientras
  el daemon escuche; eso es correcto, no un fallo.
- `nirlockctl probe` ve `/etc/nirlock/hw`, `/usr/share/nirlock/hw` y **un
  solo** perfil embebido (`hw/3277-0055.toml`, vía `include_str!`). Para que
  vea los del repo: `NIRLOCK_HW_DIRS=hw target/release/nirlockctl probe` —
  es lo que usa la revisión previa del instalador.
- Un daemon lanzado desde una llamada de herramienta muere al terminar esa
  llamada; usa el modo en segundo plano.

## Semántica de salida del daemon

`nirlockd` distingue "esta máquina nunca va a poder" de "todavía no":

- **78** (`EX_CONFIG`): había cámaras enumeradas y ninguna sirve, o falta ONNX
  Runtime. La unidad trae `RestartPreventExitStatus=78`, así que **no** se
  reinicia. Reintentar no hace crecer un sensor infrarrojo.
- **1**: todo lo demás, incluido "sysfs no muestra ninguna cámara". Eso
  durante el arranque casi siempre significa que udev no ha terminado: el
  daemon re-escanea 10 s antes de rendirse y luego sale 1 para que systemd
  reintente. `After=systemd-udev-settle.service` **no** protege de esto: esa
  unidad es `static` y nada la mete en la transacción de arranque.

Si alguna vez se amplía la lista de errores "permanentes", la pregunta a
responder es: ¿puede esto ser transitorio en el arranque? Si sí, no es 78.

## Convenciones

- Código, comentarios, README y ADRs en **inglés**; `DESIGN.md`,
  `DECISIONS.md`, la guía de instalación y los **mensajes de commit** en
  español.
- Commits: título corto en español, lenguaje natural ("Una vía para que la
  gente mande su cámara"), cuerpo que explica **el porqué** y el incidente que
  lo motivó.
- Comentarios densos que cuentan el incidente, con fecha: "medido el
  2026-09-27: ...". Mantener ese estilo; es lo que hace el código legible
  meses después.
- Decisiones de política del usuario → `DECISIONS.md`. Decisiones de diseño →
  nuevo ADR en `docs/adr/`.
- Backup en GitHub siempre; los commits los hace Claude, no el usuario.

## Estado

*Actualizado: 2026-10-03, desde el Zenbook.*

Funciona a diario en el Zenbook: 0,5–1,7 s. Verificado en **una** sola cámara.

**Primer usuario externo (Discord, 2026-10-03):** ThinkPad `5986:212b`, webcam
solo RGB, sin sensor IR, así que nunca iba a funcionar. Destapó cuatro bugs
nuestros, todos arreglados en `fix/install-preflight-and-pam` (issue #1, PR #2):
README pedía un paquete mal elegido, el instalador no comprobaba nada antes de
instalar, el daemon quedaba en bucle de reinicios, y faltaba instalar
`pam.d/nirlock-admin` mientras `other` se escribía a mano recortado.

Esa rama se revisó con 46 agentes antes de mergear y aparecieron 15 defectos
reales, incluido uno que el propio PR introducía: `RestartPreventExitStatus=78`
convertía una carrera de arranque en pérdida permanente del desbloqueo facial.
Arreglado antes de mergear (ver "Semántica de salida").

### Abierto

- **Prueba de foto impresa (G2)**: nunca hecha, no hay impresora. Es lo que
  bloquea cualquier idea de extender el rostro más allá de la pantalla.
- **Umbral 0,45**: en validación pasiva con el uso diario.
- **Oráculo de auditoría**: escribir la contraseña debería limpiar un bloqueo.
  Hoy solo lo limpian un reinicio o `nirlockctl reset-lockout`.
- **`reset-lockout` no hace la verificación PAM `nirlock-admin`** que pide
  DESIGN §6.1; hoy basta con root. `attest` y `delete` no están implementados.
  `nirlock-admin` ya se instala, pero todavía no lo llama nadie.
- **Perfiles de otras cámaras**: solo contribuidos y verificados por su dueño,
  vía las plantillas de `.github/ISSUE_TEMPLATE/`.
- `Verdict::Supported` del probe se decide por `vendor:product`; el daemon aún
  puede rechazar la cámara después (p. ej. sin nodo de metadatos).
