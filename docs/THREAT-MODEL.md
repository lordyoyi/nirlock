# nirlock — modelo de amenazas v1 (lock screen, factor de conveniencia)

Estado: diseño, 2026-09-23. Acompaña a `DESIGN.md`; su contenido pasa casi íntegro a `SECURITY.md`/`THREATS.md` del repositorio publicado.

## 1. Qué protege y qué no

**Activos**: (A1) la sesión gráfica bloqueada del usuario; (A2) las plantillas biométricas (embeddings, nunca imágenes); (A3) los contadores de bloqueo y frescura; (A4) los frames IR durante una ráfaga; (A5) la integridad del lock screen (que exista y bloquee).

**Clasificación**: el rostro es un **factor de conveniencia (Clase 2)**: FAR ≤ 1e-4 no está demostrada (E5: FAR ≤ 1e-3 por foto única en luz visible con 4 875 identidades; 0 de 14 identidades NIR), la foto impresa no se ha probado. Nunca desbloquea LUKS, el keyring, sudo, polkit ni sesiones remotas; nunca autoriza cambios de enrolamiento.

**Fuera de alcance (no mitigado, documentado)**: root; kernel; firmware de la cámara; interposición física en el puerto USB interno (CyberArk 2021 con hardware); máscaras 3D; código del mismo uid que actúa dentro del lock screen (un plugin `clonedFrom: omarchy.lock` puede llamar `finishUnlock()`; Omarchy/Hyprland ya lo permiten y nirlock no lo cambia).

## 2. Fronteras y supuestos verificados

| Supuesto | Estado |
|---|---|
| El firmware estroboscopea y etiqueta cada frame con `FrameIllumination` en D0 | [M] 480/480; no garantizado por la spec (riesgo 5 en DESIGN) |
| Las pantallas OLED/LCD no emiten en NIR | [M] 0 candidatos en 118 frames con un OLED; LCD sin medir (misma física) |
| `SO_PEERCRED` da el euid del par | [V] |
| journald marca `_TRANSPORT=audit` solo en registros del kernel; `AUDIT_USER_AUTH` exige `CAP_AUDIT_WRITE` | [V] |
| `unix_chkpwd` audita el éxito cuando `ruid != 0` | [V] fuente upstream |
| El journal es legible por `wheel` (uid 1000 pertenece) | [V] → no hay puntajes en el journal |
| `pam_start()` consulta `/etc/pam.d/<svc>` y luego `/usr/lib/pam.d/<svc>`; `pam_start_confdir(dir)` consulta **solo** `dir/<svc>` y luego `dir/other`, sin fallback al directorio de proveedor | [V] shim `LD_PRELOAD` sobre `fopen`, 2026-09-23 |
| Quickshell 0.3.1 arranca todo `PamContext` con `pam_start_confdir` y `configDirectory` por defecto `/etc/pam.d`; el binario no importa `pam_start` | [V] `/usr/bin/quickshell`, qmltypes → el lane vive en `/usr/lib/nirlock/pam.d` y el wrapper fija `configDirectory` (DESIGN §4.4) |
| `omarchy-apply-lock` solo escribe `-password` y `-fingerprint` | [V] |
| Quickshell no llama `pam_acct_mgmt` | [V] |
| Arrancar exige la passphrase LUKS (raíz en `crypt`, sin tokens) | [V] hoy; el setup lo re-verifica con `luksDump` |

## 3. Tabla de amenazas

| # | Atacante / escenario | Mitigación v1 | Residual |
|---|---|---|---|
| T0 | **Desbloqueo por proximidad**: alguien pulsa una tecla mientras el dueño está a ≤ ~80 cm mirando (por encima del hombro, dormido, distraído) | Ninguna: factor pasivo sin gesto de intención. `multiple_faces` solo actúa con dos caras creíbles en el encuadre | **Alto por diseño**; el dueño ve el desbloqueo. Lista de aceptación 7c mide la distancia a la que `small_box` deja de proteger |
| T1 | Foto/vídeo en teléfono o tablet | Física (sensor con filtro NIR; pantallas sin emisión NIR). Decisión solo con IR | Un OLED medido; LCD con retroiluminación anómala |
| T2 | **Foto impresa** (láser/inyección/brillante) | Sin probar (G2). Clase 2: bloqueo blando a 5 fallos (60 s ×2, tope 900 s), duro a 15 desde el último SAE, frescura 24 h, ≤ 8 ráfagas por lock, ≤ 36 s cámara/5 min, ≤ 600 frames puntuados/h, solo lock screen | **Alto**: literatura 97–100 % contra reconocimiento NIR. Cues deny-only (pupila brillante) solo en sombra hasta medir |
| T3 | Captura NIR dirigida + impresión / máscara | Igual que T2 | Alto / no defendido |
| T4 | **Evasión de la contabilidad**: cortar la ráfaga (tapa, EOF) o inclinar el artefacto para no llegar al presupuesto | La carga se hace al **primer frame puntuado bajo umbral** y se persiste antes del `result`; toda terminación se clasifica por frames; la expiración del bloqueo blando no reinicia el contador; máximo 15 fallos entre SAE (o entre `accept` genuinos: un `accept` reinicia los contadores, ADR-0009) | Un `no_face` puro no cuenta (sin evidencia puntuable, no hay ganancia) |
| T5 | **Malware uid 1000**: quiere un `accept`, las plantillas, reiniciar bloqueos, o usar la cámara | Autorización por `SO_PEERCRED`; el `accept` va solo a la conexión que lo pidió, ligado al nonce; plantillas y contadores en `/var/lib/nirlock` 0700 de `nirlock`; SAE solo por boot/LUKS, root con PAM propio, o registro de auditoría del kernel; puntajes nunca salen del daemon; cupos por uid | **Puede** pedir `verify` en cualquier momento (sesión desbloqueada incluida) y usar cámara + plantilla como oráculo de presencia/identidad (`match`/`no_match`/`no_face`, sin puntajes) dentro de los cupos; puede provocar bloqueos DoS del rostro con un extraño delante; puede reescribir el plugin QML (= matar el lock, preexistente) |
| T6 | Oráculo de puntajes (hill-climbing de un artefacto) | Ningún puntaje en journal, `progress`, `status` ni `result`; solo `audit.jsonl` 0600 | Root |
| T7 | Cámara falsa / reinyección (`v4l2loopback`, clon USB, `LD_PRELOAD`) | sysfs: driver `uvcvideo`, `3277:0055`, `removable=fixed`, ruta USB exacta y única; `QUERYCAP.driver` en cada open; formato exacto; alternancia estricta por metadato sobre `sequence` consecutivos (los cruces FID/brillo son solo sombra hasta E13); `LD_PRELOAD` solo afecta a clientes uid 1000, nunca al daemon | Interposición física / firmware |
| T7b | **XU Realtek (unidad 4) por la función RGB**: la misma unidad de extensión del fabricante existe en la interfaz RGB (`/dev/video0`), que sigue siendo escribible por uid 1000 aunque un día se retire `uaccess` del nodo IR. Se desconoce si escrituras ahí afectan a la función IR (exposición, estrobo, metadato) | Ninguna en v1: el daemon no escribe controles (ADR-0005) y no puede impedir que otros lo hagan; un cambio de formato o la pérdida del metadato se detectan y fallan cerrado (`camera_format`, `metadata`); un cambio de exposición solo degrada (más `saturated`/`underexposed`, nunca `accept`) | **Sin medir**: un proceso uid 1000 podría degradar o manipular la captura IR por esa vía. No se experimenta en v1 (riesgo de brickeo documentado en ADR-0005); v1.1 solo con lectura de los descriptores (E16) |
| T8 | Manipulación de plantillas (ERNW 2025) | Solo root, vía daemon, con verificación PAM del usuario (ignora la caché de sudo); sin plantillas adaptativas; hashes de modelo en el manifiesto | Root |
| T9 | Robo con equipo apagado / disco extraído | LUKS; plantillas son embeddings | Fuga de datos biométricos si LUKS cae — aceptado |
| T10 | Reinicio para obtener intentos nuevos | Con LUKS por passphrase el arranque **es** un SAE legítimo; con TPM/keyfile el setup escribe `boot_is_strong_auth=false` y nada se reinicia al arrancar | Cambio manual posterior sin `doctor` |
| T11 | Ataque al lane PAM / fail-open | Lane verificado en pam 1.7.2: `[success=done maxtries=die default=ignore]` + `required pam_deny`, sin `system-auth` ni `include` alguno, módulo sin `PAM_IGNORE` (poison), archivos del paquete en `/usr/lib/nirlock/pam.d/` (`nirlock-lock`, `nirlock-admin`, `other` de `pam_deny`) leídos por `pam_start_confdir` con ese directorio; wrapper armado solo si el archivo que su `PamContext` va a leer es byte-idéntico | Un plugin uid 1000 puede cambiar `configDirectory` en su propio proceso (ya puede llamar `finishUnlock()`: preexistente) |
| T12 | Lane ajeno en `/etc/pam.d/omarchy-lock-face` (upstream Howdy) | Sin efecto sobre nuestro lane (`/etc/pam.d` no se consulta para `nirlock-lock`); el wrapper cede solo si `faceConfigured === true`; `doctor` informa; nunca sobrescribimos | Doble escaneo si el usuario instala Howdy además: el wrapper cede |
| T13 | Actualización de Omarchy/Quickshell que rompe el wrapper → **sin lock screen, suspende expuesto** | Wrapper mínimo de dos Loaders; autocuración si el original no carga; doctor diferido tras `omarchy-update`; hook post-boot; recuperación TTY; `nirlock-remove` deshabilita antes de borrar; nada corre con la sesión bloqueada | Ventana entre el reinicio del shell y el doctor diferido (≤ 60 s); `pacman -Syu` fuera de `omarchy-update` |
| T14 | Cliente PAM hostil (basura, líneas largas, otro usuario, replay de nonce, versión) | Esquema estricto, 8 KiB, charset, nonce único, eco de `user`, `error` + cierre, fuzzing | — |
| T15 | DoS de cámara (app sostiene `/dev/video2` por uaccess/WirePlumber) | `camera_busy` → deny; contraseña intacta | Rostro no disponible hasta liberar; E11 en v1.1 |
| T15b | **Lente RGB tapada** (pegatina, obturador) con el RGB concurrente como palanca de exposición: en el hermano 0059 se reporta un estado de privacidad del firmware que deja la IR en blanco cuando la RGB está a oscuras; en este 0055 no se ha probado (E2) | Ninguna en v1; fallo cerrado (`no_face`/`underexposed`, sin fallos contados) | **Sin medir**: E2-lite en M2 (cinta sobre la lente RGB, IR con `rgb_assist=always`). Si el firmware entra en ese estado, `rgb_assist` pasa a `auto`/`off` y se documenta "no tapes la RGB si quieres rostro" |
| T16 | Flood de conexiones / mensajes / slowloris | ≤ 8 por uid, ≤ 64, ranura reservada, token bucket, línea completa en 1 s, ocioso 30 s | Rostro no disponible → contraseña |
| T17 | Manipulación del reloj | Temporizadores en `CLOCK_BOOTTIME`; estado por `boot_id` | — |
| T18 | Fuga de memoria (coredump, swap) | `LimitCORE=0`, `PR_SET_DUMPABLE=0`, `mlock` de plantillas, `zeroize`, `ProtectProc=invisible`, `MemoryDenyWriteExecute` | Swap bajo LUKS aquí; en otras instalaciones documentar |
| T19 | NIR ambiental (sol, halógeno) | Etiquetas solo por metadato (nunca por brillo); los cruces FID/brillo son **sombra** (`audit.jsonl`), no reglas, hasta que E13 (M2) mida su tasa de desacuerdo con luz ambiental | Un print a pleno sol en frames oscuros: sin medir (E13); un usuario genuino junto a una ventana no debe recibir `unavailable metadata` por un cruce no validado |
| T21b | Cuenta deshabilitada en nirlock (`set_enabled` false en todas las plantillas) o con shell `nologin` | `unavailable disabled` / `unavailable account_locked` (PROTOCOL §6.1) | Igual que T21 para el bloqueo solo por hash |
| T20 | Modelo sustituido | `/usr/share/nirlock/models` root, SHA-256 en paquete y verificado al cargar, plantillas ligadas a hashes | Root |
| T21 | Cuenta bloqueada (`passwd -l`, `chage -E`) | Quickshell no corre `pam_acct_mgmt`; el daemon rechaza si el shell es `nologin`/`false` | Una cuenta bloqueada solo por hash sigue pudiendo desbloquear **una sesión ya viva** por rostro |
| T22 | Suspensión con petición en vuelo | `PrepareForSleep(true)` cancela y cierra la cámara; tras resume, re-descubrimiento | — |
| T23 | Capa gráfica sobre `ext-session-lock` (si se habilita `above_lock`) | v1 no la usa; si se usa, cualquier cliente podría pintar sobre el lock → reportar a Hyprland | Decisión del usuario |
| T24 | Anfitriones PAM con euid 0 en v2 (sudo, polkit helper con socket 0666) | Autoridad por (uid, `client`): `pam` nunca administra; `verify` de uid 0 exige `ruser`; E10 verifica el cliente en el sandbox de polkit | v2 exige además un gesto de intención (no diseñado aquí) |

## 4. Análisis de forja desde uid 1000, mensaje por mensaje

| Mensaje | Qué obtiene un proceso hostil del usuario |
|---|---|
| `verify` propio | Un escaneo de quien esté delante y un veredicto **solo para ese proceso** (ya es uid 1000: nada que ganar); consume cupos y puede acumular fallos si hay un extraño (DoS del rostro; queda la contraseña) |
| `lock_session`, `prewarm`, `subscribe` | Solo cuándo se calienta el daemon y el presupuesto de cortesía por lock; RAM |
| `cancel` | Solo con el nonce de su propia petición |
| `status` | Su propio estado sin edad del SAE ni puntajes |
| Líneas al journal | No puede fijar `_TRANSPORT=audit` ni emitir `AUDIT_USER_AUTH` (sin `CAP_AUDIT_WRITE`); `SCM_CREDENTIALS` con uid 0 exige `CAP_SETUID` |
| Editar `~/.config/omarchy/plugins/nirlock.lock/` | Sí, como cualquier plugin: puede hacer que el lock se abra solo. Preexistente. El daemon nunca confía en el wrapper |
| `pam_start_confdir` con un lane propio sin `pam_deny` | Solo afecta a su propio proceso |
| Sostener la cámara | DoS (fail-closed) |
| Forzar reinicio del daemon (pánico por entrada, 300 s sin lock) | Nada: los contadores persisten; `boot_id` no cambia |

## 5. Fallo cerrado, ruta por ruta

| Fallo | Dónde | Resultado |
|---|---|---|
| Socket ausente / rechazado / sin `welcome` en 2,5 s | módulo | `AUTHINFO_UNAVAIL` → `pam_deny` |
| Daemon muere a mitad (EOF) | módulo | `AUTHINFO_UNAVAIL` → deny |
| `.so` ausente | libpam | módulo desconocido → ignore → `pam_deny` (verificado: nunca 0) |
| Lane ausente en `/usr/lib/nirlock/pam.d` | libpam | `<confdir>/other` (nuestro, `pam_deny`) → deny; sin `other` tampoco, libpam sin configuración deniega (verificado con `/etc/pam.d/other`: 7) |
| `configDirectory` no fijado en el plugin (regresión) | libpam | `/etc/pam.d/nirlock-lock` inexistente → `/etc/pam.d/other` → deny (verificado: 7); `doctor` lo detecta |
| Lane ajeno en `/etc/pam.d/omarchy-lock-face` | — | no se lee para `nirlock-lock`; sin efecto |
| Módulo de otro `proto` | módulo | `SERVICE_ERR` → deny |
| Cámara ausente / no fijada / ocupada / formato / metadato | daemon | `unavailable` → deny |
| Modelo ausente / hash distinto / ORT no carga | daemon | `unavailable models_unavailable` → deny |
| Plantilla ausente / hash distinto | daemon | `unavailable not_enrolled|template_stale` → deny |
| Config ilegible | daemon | exit 78; socket escucha; módulo → `AUTHINFO_UNAVAIL` en ≤ 2,5 s |
| `state.json` corrupto / `boot_id` ilegible | daemon | bloqueo duro hasta SAE root; marca de agua del oráculo = ahora |
| Kernel < 6.17 (sin `UVCM`) | daemon | `S_FMT` rechazado → `unavailable` |
| Reset-resume USB cambió los nodos | daemon | re-descubrimiento; sin coincidencia → `unavailable` |
| Tapa cerrada / suspensión | daemon | `unavailable lid_closed|suspending` (contabilizado si ya había frames puntuados) |
| Oráculo no disponible | daemon | solo SAE boot/root: más estricto, nunca más laxo |
| Hijo PAM muerto (`abort`) | daemon | cancelado, cámara cerrada, contadores ya persistidos |
| Wrapper roto | shell | carril inerte o (peor caso) sin lock hasta autocuración/doctor; nunca un desbloqueo falso |
| Pánico | daemon | abort; EOF a todos → deny; reinicio sin cambio de contadores |
| Dos caras / cara + impresión | gate | `multiple_faces` → fallo |
| Cupo excedido | daemon | `unavailable` → deny (no cuenta como fallo) |

**Invariante**: hay exactamente un camino a `PAM_SUCCESS`: una línea `result` con `outcome:"accept"`, nonce y usuario idénticos a la petición, producida por el daemon tras K=2 de W=4 aciertos sobre frames etiquetados por metadato de la cámara fijada, con todas las reglas de política satisfechas.

## 6. Lo que SECURITY.md debe decir en la primera pantalla

1. Es un factor de conveniencia. La contraseña siempre funciona y es la credencial.
2. No se ha probado contra fotos impresas; por eso no toca sudo, polkit, LUKS ni el keyring.
3. No hay comprobación de atención: puede desbloquear delante del dueño sin que lo pretenda.
4. Cualquier programa que corra como el usuario puede pedir un escaneo (sin ver puntajes ni imágenes) y ya podía, sin nirlock, abrir el lock screen de Omarchy desde un plugin.
5. Root, kernel y firmware están fuera del modelo.
6. Actualizar Omarchy fuera de `omarchy-update` salta la comprobación automática del plugin; `nirlockctl doctor` la ejecuta a mano.
