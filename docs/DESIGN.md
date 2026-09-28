# nirlock — diseño v1 (lock screen de Omarchy, Zenbook UX3405CA)

Estado: **diseño de síntesis, 2026-09-23, revisado tras la lista de correcciones del mismo día** (§14 resume qué cambió y qué huecos de investigación siguen abiertos). Sustituye a `design/DESIGN-v1.md` (propuesta ux-latency-first, que se conserva solo como referencia histórica y lleva un aviso de superación en su primera línea). Documentos normativos que lo acompañan: `design/PROTOCOL.md` (protocolo de cable), `design/THREAT-MODEL.md` (modelo de amenazas) y `design/adr/` (decisiones).

Alcance v1: **solo lock screen**. sudo/polkit quedan para una versión posterior, condicionados a la prueba de foto impresa (G2) que aún no se puede correr. Proyecto publicable, **MIT** (decisión del usuario, 2026-09-23), modelos de licencia limpia (MIT / Apache-2.0, con sus avisos en `THIRD-PARTY.md`), nada copiado de proyectos GPL-3.

Convención: **[M]** = medido en esta máquina (Fase 0); **[V]** = verificado en archivos instalados o fuente primaria; **[H]** = hipótesis o cálculo, pendiente de medir.

---

## 0. Nombre y resumen ejecutivo

**Nombre: `nirlock`.** Comprobado el 2026-09-23 (esta síntesis, además de las tres propuestas): crates.io 404, AUR 0 resultados, GitHub 0 repos con ese nombre, PyPI 404. Deriva: `nirlockd` (daemon), `pam_nirlock.so`, `nirlockctl` (CLI), `nirlock.lock` (plugin), `/run/nirlock/sock`, `/var/lib/nirlock`, `/etc/nirlock`. Alternativas descartadas: `strobeface` (jocoso para un componente de autenticación), `zenface` (ata el proyecto al Zenbook; colisiones menores en GitHub), `facegate` (ya existe en AUR), `mirada`/`semblance` (crates reservados en 2026).

En una frase: un daemon Rust sin root y con sandbox es el único que abre la cámara IR, etiqueta cada frame con el metadato `FrameIllumination` del firmware, reconoce con AuraFace (K=2 de los últimos W=4 frames iluminados) y responde a un módulo PAM en C de 300 líneas; un plugin de Omarchy envuelve el lock screen original sin modificarlo y añade un carril `PamContext` de rostro. La contraseña siempre funciona; todo fallo termina en `pam_deny`.

Qué se tomó de cada propuesta:

| Fuente | Se adopta |
|---|---|
| **security-first** (estructura base) | NDJSON, `SO_PEERCRED`, dos clases de contadores, oráculo de auditoría (`unix_chkpwd` → `AUDIT_USER_AUTH`), sandbox completo, activación por socket con pre-calentamiento en el lock, tabla de códigos PAM, matriz de carreras, `nirlock-setup`/`doctor`, plan de pruebas y replays |
| **ux-latency-first** | Línea de tiempo por caso, disparo por `IdleMonitor`, `failureMessage` como único canal de feedback v1, `nonce` por request, presupuesto de cámara por ventana de 5 min, caducidad de hits (800 ms, definida sobre la marca de llegada del frame), orden de arranque RGB→IR, MSRV/`ort` fijados, `lsblk`/LUKS en el setup. Su estado `HOLD` (cámara abierta 400 ms tras la petición) **no se adopta**: era inalcanzable bajo `verify_min_interval_ms = 1500` y encendía el emisor sin petición en vuelo (§2.8) |
| **maintainability-upstream** | Perfil de hardware TOML, usuario estático `nirlock` (sysusers), gate de enrolamiento más estricto (solo como opción apagada hasta validarlo por replay, §2.5), `strong_auth_reset_on_boot` decidido en el setup, `docs/COMPAT.md`/`HARDENING.md`, hook `post-boot.d`, scripts sin prefijo `omarchy-`, `stale` → `AUTHINFO_UNAVAIL`, camino de coexistencia con upstream |

Hallazgos de las nueve revisiones: todos los bloqueantes y mayores están resueltos en el texto; la tabla del Apéndice A dice dónde, y qué hallazgos se rechazan y por qué.

---

## 1. Visión general

### 1.1 Componentes

| # | Componente | Corre como | Lenguaje / tamaño | Confianza |
|---|---|---|---|---|
| 1 | `nirlockd` | servicio de sistema, usuario estático `nirlock` (+ grupo `video`), activado por socket, sandbox | Rust, ~6 k líneas (port de `phase0/v4l2cap.*` y `phase0/pipeline.*`) | Único que toca cámara, modelos y plantillas. Decide. Posee todos los contadores. |
| 2 | `pam_nirlock.so` | dentro del anfitrión PAM (v1: hijo bifurcado de Quickshell, uid 1000) | C11, ~300 líneas, solo libc + libpam | Mensajero. El daemon verifica todo lo que dice por `SO_PEERCRED`; el `accept` va ligado a la conexión y al `nonce`. Nunca `PAM_IGNORE`. |
| 3 | `nirlock.lock` | plugin de Quickshell (`omarchy-shell`), uid 1000 | QML, ~300 líneas: `Loader` del `Service.qml` original + `Loader` de `FaceLane.qml` | No es de confianza para el daemon. Solo UX y política de disparo. |
| 4 | `nirlockctl` | usuario (`status`, `doctor`) o root (`enroll`, `delete`, `attest`, `reset-lockout`, `template`, `bench`, `record`, `plugin`) | Rust, comparte la crate `nirlock-wire` | Los verbos root se autorizan por uid 0 + `client=ctl` **y** una verificación PAM propia (`nirlock-admin`) que ignora la caché de sudo. |

Archivos root: `/usr/lib/nirlock/pam.d/{nirlock-lock,nirlock-admin,other}` (directorio PAM propio del paquete, §4.4), `/etc/nirlock/config.toml`, `/usr/share/nirlock/models/` (SHA-256 fijados), `/usr/share/nirlock/hw/3277-0055.toml`.

### 1.2 Fronteras de confianza

- **B1 uid 1000 ↔ nirlockd** (`/run/nirlock/sock`, `SO_PEERCRED`). Todo lo que cruza desde uid 1000 es *pista* o *petición*. Un par uid N solo puede `verify` al usuario N, y solo leer su propio estado. Ningún campo enviado por uid ≠ 0 entra en la política salvo `user` (contrastado con el uid del par); `client`, `service`, `trigger`, `budget_ms` son auditoría o se acotan.
- **B2 uid 0 ↔ nirlockd.** Solo `client=ctl` en una conexión que nunca envía `verify`; enrola, borra, atesta, reinicia lockouts. Un anfitrión PAM que corra como euid 0 (sudo, polkit en v2) sigue siendo `client=pam` y no obtiene verbos de administración.
- **B3 kernel ↔ nirlockd.** sysfs (identidad de cámara), V4L2, journald (registros `_TRANSPORT=audit` que uid 1000 no puede forjar), logind por D-Bus (`PrepareForSleep`, `LidClosed`, sesiones).
- **B4 paquete `omarchy` ↔ todo bajo `/usr/share/omarchy` y `/etc/pam.d/omarchy-lock-*`.** No poseemos nada ahí. Nuestro lane vive en un directorio PAM propio, `/usr/lib/nirlock/pam.d/`, que el `PamContext` del wrapper selecciona con `configDirectory` (§4.4): ningún archivo de `/etc/pam.d` puede sustituirlo ni anularlo, y el wrapper se arma solo si el archivo que ese `PamContext` va a leer coincide byte a byte con el esperado.
- **Fuera de alcance (documentado, no mitigado):** root, kernel, firmware, interposición física en el bus, y código del mismo uid *dentro* del lock screen: la integridad del lock screen frente a código uid 1000 es la de Omarchy/Hyprland (un plugin `clonedFrom: omarchy.lock` puede llamar `finishUnlock()`); nirlock no la mejora ni la empeora. Lo que sí protege frente a uid 1000: plantillas, contadores, frescura, enrolamiento, imágenes.

### 1.3 Diagrama

```
      uid 1000 (no confiable para autorización)              |   servicios de sistema (usuario nirlock, sandbox)
                                                              |
  omarchy-shell (Quickshell 0.3.1)                            |
  +-----------------------------------------------------+     |   /run/nirlock/sock  (0666 root:root, SO_PEERCRED)
  | nirlock.lock  (~/.config/omarchy/plugins/nirlock.lock)|    |   +---------------------------------------------+
  |  Service.qml (wrapper, 2 Loaders)                     |    |   | nirlockd                                    |
  |   Loader A -> stock lock/Service.qml (password lane,  | control (NDJSON)|  ipc: hello/verify/prewarm/status/lock_session |
  |              fingerprint lane, WlSessionLock, LockView)|<------->|  policy: lockout, frescura, presupuestos, seat |
  |   Loader B -> FaceLane.qml                            |    |   |  camera: sysfs pin, V4L2 GREY+UVCM, RGB       |
  |     IdleMonitor (actividad)  Socket (control)         |    |   |  pipeline: YuNet -> gate -> align ->           |
  |     FileView (lane)  PamContext "nirlock-lock"        |    |   |            AuraFace -> max-cos -> K=2/W=4      |
  |       configDirectory=/usr/lib/nirlock/pam.d          |    |   |  state: /var/lib/nirlock (plantillas,          |
  +-------------------|-----------------------------------+    |   |         state.json, audit.jsonl)               |
                      | fork (uid 1000)                        |   +------+-------------+-------------+-----------+
        +-------------v-----------------+   verify / result    |          |             |             |
        | hijo PAM: libpam              |<-------------------->|          |             |             |
        |  /usr/lib/nirlock/pam.d/nirlock-lock                 |   /dev/video2+3    journald      logind (D-Bus)
        |  pam_nirlock.so  ->  pam_deny.so                     |   (IR+UVCM)        _TRANSPORT=   PrepareForSleep
        +-------------------------------+                      |   /dev/video0      audit         LidClosed
                                                               |   (RGB, palanca)   USER_AUTH     ListSessions
  sudo nirlockctl enroll|attest|reset-lockout (uid 0, client=ctl) --+
```

### 1.4 Flujo de un desbloqueo

1. **Lock.** `lockRequested` → true: el wrapper conecta el socket de control, envía `hello client=lock`, `subscribe` (todos los eventos), `lock_session locked` y `prewarm`. El daemon (activado por socket si no estaba) carga YuNet + AuraFace (~570 ms con OpenCV [M]; con ORT sin medir, 0,6–0,9 s [H]) y verifica los SHA-256 una vez por vida del proceso. Sin cámara, sin escaneo.
2. **Disparador.** Actividad (`IdleMonitor` idle→activo), reanudación (`event resumed` del daemon, desde logind) o apertura de tapa (`event lid_open`), siempre con la sesión ya `secure` y tras 3 s de gracia desde el `secure=true`. El wrapper llama `facePam.start()`; Quickshell bifurca un hijo que corre `pam_start_confdir("nirlock-lock", user, conv, "/usr/lib/nirlock/pam.d")` + `pam_authenticate` (Quickshell **solo** usa `pam_start_confdir`, con `/etc/pam.d` por defecto; el wrapper fija `configDirectory`, §4.4).
3. **verify.** `pam_nirlock.so` conecta, envía `hello`, espera `welcome`, y envía `verify user=rodrigo lane=lock nonce=… budget_ms=…` (presupuesto calculado desde su plazo restante, §4.2). El daemon comprueba: uid del par ↔ usuario, sesión local activa en `seat0`, enrolado, hashes de modelos y plantilla, no bloqueado, frescura, tapa abierta, cámara libre, límites. Arranca el hilo RGB, abre IR+meta, STREAMON, hasta 4 000 ms.
4. **Por frame iluminado:** etiqueta del metadato → chequeo de buffer → YuNet → gate → alineación 5 puntos → AuraFace 512-d → coseno MÁXIMO sobre las filas de plantilla → ventana K=2/W=4. Cada frame que pasa el gate y queda bajo el umbral se **contabiliza antes** de seguir (§6.3).
5. **accept** → `result` → `PAM_SUCCESS` → `[success=done]` → `PamResult.Success` → el wrapper llama `stock.item.finishUnlock()` (aborta contraseña y huella). Cualquier otro resultado → deny; el wrapper decide por el evento `verify_finished` si reintenta.

### 1.5 Línea de tiempo objetivo

Constantes [M]: open→STREAMON 113 ms; primer frame (siempre iluminado) 253 ms desde `open()` con USB autosuspendido, ~180 ms si estaba despierto; un frame iluminado cada 133 ms. **En batería (M0b, `onnxruntime-cpu` 1.29.0 de Arch, 2026-09-23): YuNet 8,4 ms; AuraFace a 4 hilos 149 ms/frame (p90 179; 8 hilos 115; 1 hilo 378), primera inferencia 184 ms, carga 656 ms; SFace 25 ms; RSS 490 MiB, pico 552 MiB. Es 2,5–3× lo medido en AC y coincide con la Fase 0 (OpenCV, batería): el runtime no es la variable, la energía sí.** Inferencia con ONNX Runtime 1.29.1 (`ort` rc.13, `load-dynamic`), medida en M0 en esta máquina, en AC (`docs/BENCH.md`, 2026-09-23; 75 frames iluminados de `enroll-a`, sesiones cargadas en orden daemon, bucle cerrado de 40 iteraciones, tres corridas intercaladas con OpenCV): YuNet `intra_op=1` **3,9–4,5 ms** (primera inferencia 4,1–4,7); AuraFace fp32 a 4 hilos **55–72 ms/frame** (mediana de tres corridas 69, p90 62–84; a 8 hilos 51–54; a 1 hilo 174–192), **primera inferencia en frío +1–20 ms** (70–74), carga **325–343 ms** sin forward de calentamiento; SFace sombra **9–11 ms** a 4 hilos; RSS con los tres modelos residentes **~490 MiB, pico 550 MiB**; con `MemoryDenyWriteExecute` idéntico. Referencia OpenCV 5 `cv::dnn` **like-for-like** (mismos frames, hilos y estado de energía, corridas intercaladas): AuraFace 55–62 ms a 4 hilos, 51 a 8, 178–190 a 1 — **ORT no es más rápido que OpenCV en AuraFace** (igual dentro del ruido a 1/2/8 hilos, 0–15 ms más lento a 4) y es ~2,5× más lento en SFace; su justificación sigue siendo la clausura de dependencias (ADR-0015). Referencia OpenCV de fase 0 **en batería**, dentro del bucle de cámara de fuprobe: AuraFace ~150 ms/frame, carga 570 ms, YuNet ~10 ms, SFace ~16 ms. **La cifra en batería con ORT sigue pendiente** (la máquina estuvo en AC durante M0; Omarchy cambia `platform_profile` en batería): como los dos runtimes coinciden en AC, se espera que aterrice cerca de los ~150 ms de fase 0, y hasta medirla la tabla siguiente conserva ese valor como referencia conservadora. La investigación previa (ORT 1.30, tarball) había dado 94–107 ms/frame en AC. Cifras [M] en AC, [H] en batería:

| Caso | Secuencia | K=2 esperado | Medido de referencia |
|---|---|---|---|
| B — tecla/trackpad, cámara suspendida, modelos tibios | +15 verify → +128 STREAMON → +268 F0 → det +283 → embed(F0) +460 (primera inferencia lenta) → F1 +401, embed desde +460 → **+610** | 610–720 ms mediana (batería, ~150 ms/embed [H]); en AC con ORT a 4 hilos (~70 ms/embed, +20 la primera) el mismo esquema da ≈ +283 det → embed(F0) +355 → F1 +401 → embed +471 → **≈ +475** [M en componentes, sin medir de punta a punta] | 634 ms (OpenCV, tibio) / 596 ms con RGB [M] |
| A — apertura de tapa / resume | igual que B, pero la cámara acaba de reenumerar (USB reset-resume); el daemon espera a que el nodo fijado reaparezca (≤ 2 s) antes de abrir | **sin medir** (E7 pendiente) | — |
| C — pieza a oscuras | la AE puede quemar la cara 1–2 s; el RGB concurrente la deja limpia (media de cara 112, saturación 0,001, 8/8) | ≤ 1 000 ms máx con RGB [M] | K=2 máx 1 000 ms con RGB; 1 326–1 882 sin RGB [M] |
| Modelos fríos (daemon recién activado, sin prewarm) | la cámara se abre **en paralelo** con la carga; se descartan frames hasta que los modelos están | ~985 ms | 985 ms [M] |

Palancas que sí bajan el piso (M0/M2): forward ficticio en ambas sesiones al recibir `verify` (rampa de frecuencia y migración a P-cores durante los 250 ms de espera de F0; M0 midió +1–20 ms de primera inferencia en frío y un bucle a cadencia de 133 ms dentro del rango del bucle cerrado, así que el coste real está en el primer frame tras segundos de reposo, que se mide en M3 con el bucle real); YuNet con `intra_op=1` (confirmado: 3,9–4,5 ms); SFace sombra **después** de actualizar la ventana (9–11 ms bajo ORT, 2,5× más que OpenCV: si el presupuesto aprieta, la sombra es lo primero que se mueve fuera del camino crítico); AuraFace int8 estático (AVX-VNNI presente; ~1,8× esperado, cambia los puntajes → revalidar umbral con E5). Rechazado: dos workers de embedding (≤ 19 ms de ganancia, 2× RAM); 8 hilos intra-op (−15 ms de mediana a cambio de ocupar los E-cores y el paquete entero). Memoria: `MemoryHigh=900M` (§2.2) tiene ~350 MiB de margen sobre el pico medido de 550 MiB; se puede bajar a 700M en M3.

---

## 2. Daemon `nirlockd`

### 2.1 Modelo de proceso

- Un proceso, `Type=notify`, activado por `nirlockd.socket`. **Residente mientras hay una sesión de lock registrada** (`lock_session locked` de un par del uid enrolado; la sesión dura hasta `lock_session unlocked` o hasta 300 s después de que esa conexión se cierre, PROTOCOL §3) y durante 300 s después del último `unlocked`/desconexión sin peticiones; luego sale (`ExitType=main`). Una conexión `lock` con sesión `locked` **no** está sujeta al cierre por inactividad de 30 s. La siguiente conexión lo reactiva. Los modelos se cargan en `prewarm` o en el primer `verify` y no se descargan mientras el proceso vive. Justificación: 985 vs 634 ms [M] entre modelos fríos y tibios; el lock precede al desbloqueo por segundos; ~500 MB de RSS todo el día no se justifican.
- Hilos: `control` (tokio current-thread: sockets, zbus con el bus de sistema, timers, política, watchdog), `capture-ir` (vídeo + meta, `poll()`, emparejado por `sequence`), `capture-rgb` (MJPG 1280x720, DQBUF/QBUF sin decodificar), `infer` (sesiones ORT con un pool global `intra_op=4`, `inter_op=1`, sin spinning, sin afinidad — el pinning a P-cores midió 2× peor bajo carga de escritorio). Canal capture→infer de profundidad 1, **el último gana** (150 ms > 133 ms de cadencia).
- Arranque: `umask 077`, `prctl(PR_SET_DUMPABLE, 0)` (no se llama a `setrlimit`: `LimitCORE=0` en la unidad y `@resources` está filtrado), `panic = "abort"`. Embeddings, crops y frames en buffers `zeroize` al soltar. Plantillas cargadas en memoria `mlock` (`LimitMEMLOCK=64M`).
- Watchdog: `sd_notify(WATCHDOG=1)` cada 10 s desde `control`, solo si el bucle de `control` avanzó (contador atómico), para que un cuelgue de ese hilo sí reinicie.

### 2.2 Unidades systemd (verbatim)

`/usr/lib/systemd/system/nirlockd.socket`
```ini
[Unit]
Description=nirlock face verification socket

[Socket]
ListenStream=/run/nirlock/sock
SocketMode=0666
SocketUser=root
SocketGroup=root
DirectoryMode=0755
Backlog=16
RemoveOnStop=yes

[Install]
WantedBy=sockets.target
```

`/usr/lib/systemd/system/nirlockd.service`
```ini
[Unit]
Description=nirlock face verification daemon
Requires=nirlockd.socket
After=nirlockd.socket dbus.service systemd-udevd.service
StartLimitIntervalSec=0

[Service]
Type=notify
ExecStart=/usr/lib/nirlock/nirlockd
ExitType=main
Restart=on-failure
RestartSec=1
RestartPreventExitStatus=78
TimeoutStopSec=5
WatchdogSec=30
User=nirlock
Group=nirlock
SupplementaryGroups=video systemd-journal
StateDirectory=nirlock
StateDirectoryMode=0700
ConfigurationDirectory=nirlock
Environment=ORT_DYLIB_PATH=/usr/lib/libonnxruntime.so
UMask=0077
LimitCORE=0
LimitMEMLOCK=64M
MemoryHigh=900M
MemoryMax=1200M
TasksMax=48
OOMScoreAdjust=-500
NoNewPrivileges=yes
CapabilityBoundingSet=
AmbientCapabilities=
PrivateNetwork=yes
IPAddressDeny=any
RestrictAddressFamilies=AF_UNIX
PrivateTmp=yes
DevicePolicy=closed
DeviceAllow=char-video4linux rw
ProtectSystem=strict
ProtectHome=yes
ProtectProc=invisible
ProcSubset=all
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
RestrictNamespaces=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RemoveIPC=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes
KeyringMode=private
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@privileged @resources @mount @obsolete @debug @cpu-emulation @module @raw-io @reboot @swap
SystemCallErrorNumber=EPERM
```

`/usr/lib/sysusers.d/nirlock.conf`: `u nirlock - "nirlock face unlock" /var/lib/nirlock`. `/usr/lib/tmpfiles.d/nirlock.conf`: `d /var/lib/nirlock 0700 nirlock nirlock -`.

Notas (cada una responde a un hallazgo): **sin `RuntimeDirectory=`** en el servicio (borraría `/run/nirlock` y el socket en cada parada; el directorio lo crea la unidad `.socket`). **`ProcSubset=all`** (con `pid` no se lee `/proc/sys/kernel/random/boot_id`, ni `/proc/cpuinfo` que usa `cpuinfo`, dependencia de `onnxruntime-cpu`). `ProtectKernelTunables` deja `/proc/sys` y `/sys` de solo lectura, suficiente. Tapa y suspensión llegan por logind, no por `/proc/acpi`. `StartLimitIntervalSec=0` + `RestartPreventExitStatus=78` (error de configuración: sale con 78 y no se reintenta; el socket sigue aceptando y el módulo recibe `welcome` nunca → `AUTHINFO_UNAVAIL` en ≤ 2,5 s). `Nice` eliminado (hilos de ORT por encima del compositor mientras se teclea). `MemoryHigh=900M`: el RSS con ORT no está medido; M0 lo mide y ajusta. E10 (unidad real con estas directivas, streaming + inferencia + lectura de journal + boot_id + D-Bus) es el primer criterio de aceptación de M3, no de M7.

Tabla de syscalls que el daemon usa y su grupo (`systemd-analyze syscall-filter`, para la lista de comprobación de E10): `prctl` @process (permitido); `mlock` @memlock ⊂ @system-service; `setrlimit` @resources (no se usa); `sched_setaffinity` @resources (no se usa); `memfd_create` @ipc; `inotify_*` @file-system; `ioctl`, `mmap`, `poll`, `socket AF_UNIX` en @default/@network-io.

### 2.3 Descubrimiento y fijado de la cámara

Port de `discover_camera()` + perfil de hardware `/usr/share/nirlock/hw/3277-0055.toml` (override en `/etc/nirlock/hw/`):

```toml
id = "shinetech-3277-0055"
match = { vendor = "3277", product = "0055", removable = "fixed" }
ir   = { interface = 2, index = 0, format = "GREY", width = 640, height = 360, fps = 15, bytes = 230400 }
meta = { interface = 2, index = 1, format = "UVCM" }
rgb  = { interface = 0, index = 0, format = "MJPG", width = 1280, height = 720 }
emitter = "firmware-strobe"     # único valor aceptado para autenticación en v1
labeler = "uvcm-metadata"       # único valor aceptado para autenticación en v1
notes = "Nunca escribir la XU Realtek (unidades 4/10/11) ni la interfaz DFU. v1 no escribe ningún control."
```

1. Recorre `/sys/class/video4linux/*`; un nodo califica solo si `device/driver` → `uvcvideo`, `idVendor/idProduct` coinciden con el perfil, `removable=fixed`, y la ruta sysfs del dispositivo USB coincide con la fijada en el manifiesto de la plantilla (`usb_sysfs`, p. ej. `/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9` [V]). Roles por `bInterfaceNumber` × `index`. Si en el enrolamiento hay más de un candidato → `camera_ambiguous`; el pinning exige además el nodo de metadatos hermano (`index=1`) en la misma interfaz. `bcdDevice` se guarda; un cambio solo avisa en `doctor`.
2. En cada `open`: `VIDIOC_QUERYCAP`, `driver == "uvcvideo"` exacto, capacidades esperadas; `VIDIOC_S_FMT` debe devolver exactamente GREY 640x360 / `UVCM` / MJPG 1280x720; cualquier cambio silencioso → `unavailable camera_format`.
3. ioctls hacia el dispositivo: solo `S_FMT / REQBUFS / QUERYBUF / QBUF / DQBUF / STREAMON / STREAMOFF` y las lecturas `QUERYCAP / G_FMT`. **Ningún `UVCIOC_CTRL_QUERY` en el daemon** (ni GET; `nirlockctl doctor --hw` conserva la lista blanca de `xu_get()` solo para diagnóstico). Sin `S_CTRL`, `S_EXT_CTRLS`, `S_PARM`, XU, DFU. ADR-0005.
4. Orden de apertura, **copiado de `cmd_latency` + `IrCapture::open` (`phase0/main.cpp` 1300–1306, `phase0/v4l2cap.cpp` 411–439)**, en esta secuencia exacta:
   1. `rgb.start()` — el hilo RGB arranca sin esperar (en él: `open("/dev/video0")`, `S_FMT MJPG 1280x720`, `REQBUFS 4`, `QUERYBUF/QBUF ×4`, `STREAMON`; luego `DQBUF/QBUF` sin decodificar hasta el cierre).
   2. `t0 = mono_now()` — **inmediatamente antes del `open()` del nodo de vídeo**; todas las latencias (§1.5, `bench`) se refieren a este instante.
   3. `open("/dev/video2")` (vídeo IR).
   4. `open("/dev/video3")` (metadatos). Aquí termina `open_ms` (113 ms hasta el paso 8 en el medido [M]).
   5. `S_FMT GREY 640x360` en vídeo.
   6. `S_FMT UVCM` en meta.
   7. Meta: `REQBUFS 32`, `QUERYBUF/QBUF ×32`, **`STREAMON` de meta primero** (si no, el primer frame, siempre iluminado, no tiene búfer de metadatos donde aterrizar; comentario original en `v4l2cap.cpp` 423).
   8. Vídeo: `REQBUFS 8`, `QUERYBUF/QBUF ×8`, `STREAMON`. Fin de `streamon_ms`.
   El texto anterior de este punto ("luego meta … y por último vídeo") se leía como abrir meta antes que vídeo; el orden normativo es el de arriba: se abren **vídeo y luego meta**, se formatean **vídeo y luego meta**, y se arranca **meta y luego vídeo**. RGB `EBUSY` (navegador) → se sigue solo IR, `rgb_assist=denied` en el `result` y el journal. IR `EBUSY` → 3 reintentos × 150 ms, luego `unavailable camera_busy`. Todo lo anterior (incluida la espera tras resume del punto 5) corre **dentro** del `budget_ms` de la petición; el daemon nunca gasta tiempo fuera del presupuesto antes de abrir.
5. Tras `PrepareForSleep(false)` o `LidClosed→false`: `camera_state=unknown`; el próximo `verify` re-descubre y espera hasta 2 000 ms (sondeo cada 100 ms de sysfs) a que el dispositivo fijado reaparezca antes del primer `open`. **Inhibidor de retardo**: al arrancar, el daemon toma `org.freedesktop.login1.Manager.Inhibit("sleep", "nirlock", "closing the IR camera", "delay")` por zbus (fd pasado por D-Bus; la acción polkit `org.freedesktop.login1.inhibit-delay-sleep` está permitida por defecto para cualquier sesión/servicio [H, verificar en M5 con `pkaction --verbose`]). Con `PrepareForSleep(true)`: cancela toda petición en curso (`result unavailable suspending` si había 0 frames puntuados; si no, clasificación de §2.7), STREAMOFF y cierra ambos nodos, **y solo entonces** cierra el fd del inhibidor para que logind continúe (tope de logind `InhibitDelayMaxSec`, 5 s por defecto; nuestro cierre tarda < 70 ms). Sin el inhibidor logind no espera a nadie y el STREAMOFF podría no completarse antes del s2idle, que es justo lo que se quiere evitar (nodo colgado tras reset-resume). Tras `PrepareForSleep(false)` el daemon **vuelve a pedir** el inhibidor (el fd anterior queda inválido al liberarlo). Si `Inhibit` falla (polkit, D-Bus), el daemon lo anota en el journal y sigue sin él: comportamiento degradado, no fallo.
6. Tapa: `LidClosed` de logind se lee al inicio de la petición **y en cada frame iluminado**; cierre a mitad → la petición termina (contabilizada, §6.3) con `unavailable lid_closed` y se emite `event lid_close`.

### 2.4 Etiquetado de frames

`parse_uvcm()` portado 1:1: concatenación de los blobs de todas las cabeceras, descarte de los bloques anteriores a un cambio de FID dentro del búfer, ítem `KSCAMERA_METADATA_ITEMHEADER` id 6 bit 0 = iluminado; ítem truncado o dos ítems 6 en desacuerdo → `parse_error`; `V4L2_BUF_FLAG_ERROR` en el búfer de metadatos → etiqueta desconocida. Búfer de vídeo con `V4L2_BUF_FLAG_ERROR` o `bytesused != 230400` (`nodrop=1` aquí) → inutilizable, nunca puntuado, nunca mitad oscura de un par. Frame sin etiqueta → no se usa.

**Emparejado vídeo↔meta, portado de `IrCapture::next()` (`v4l2cap.cpp` 455–500)**, sin simplificar:
- Un solo `poll()` sobre los dos fds. Cuando el fd de meta tiene `POLLIN` se drena entero (`drain_meta`: cada búfer se parsea, se anota en `meta_by_seq[sequence]` y se reencola). Después se saca **un** búfer de vídeo.
- Tras sacar el búfer de vídeo se vuelve a drenar meta y se busca `meta_by_seq[sequence]`. uvcvideo completa el búfer de metadatos justo **antes** que el de vídeo del mismo frame, así que normalmente ya está; si no está, **`poll()` de gracia de 25 ms solo sobre el fd de meta** y nuevo drenado. Solo tras esa gracia el frame se declara "sin etiqueta". Un port sin la gracia declararía sin etiqueta frames perfectamente etiquetados y dispararía la regla del 50 %.
- **Poda**: tras resolver el frame se borra `meta_by_seq` hasta `sequence` inclusive (`erase(begin, upper_bound(sequence))`). Un metadato que llegue después para una secuencia ya entregada se descarta; el mapa nunca crece más allá de los frames en vuelo.
- El primer búfer tras `STREAMON` puede llegar con FID mezclado (cabecera de un frame saltado, comentario en `v4l2cap.cpp` 197): el parser lo maneja descartando los bloques previos al cambio de FID; no es una anomalía.

**Reglas duras por petición** (fallo cerrado, ADR-0005/THREAT-MODEL; solo lo que la Fase 0 ya garantiza [M]):
1. Un frame sin etiqueta nunca se usa (ni puntuado ni como mitad oscura).
2. Si > 50 % de los frames **utilizables** de la petición no tienen etiqueta → `unavailable metadata` y fin de la petición.
3. Si dos frames con **números `sequence` de V4L2 consecutivos** (`seq` y `seq+1`, ambos entregados y etiquetados) llevan la misma etiqueta → `unavailable metadata`. La condición se evalúa sobre `sequence`, **no** sobre entregas consecutivas: un salto de secuencia (frame perdido en el kernel) entre dos entregas hace legítimo que ambas lleven la misma etiqueta y no cuenta.

**Cruces en modo sombra (no son reglas; solo `audit.jsonl`)**, hasta que E13 los valide: (a) paridad del FID frente a la etiqueta; (b) `mean(lit) > mean(dark) + margen` frente a la etiqueta. Ninguno de los dos se midió en Fase 0 (nunca se cruzaron etiquetas con FID ni con brillo), el margen de +8 y un umbral del 10 % de pares serían inventados, y con NIR ambiental (sol, halógeno: E13, sin medir) los frames oscuros pueden ser brillantes, lo que convertiría a un usuario genuino junto a una ventana en `unavailable metadata` permanente. Se registra por sesión la tasa de desacuerdo de cada cruce y se publica en `docs/BENCH.md` (M8); solo después de E13 (ventana + halógeno) puede alguno pasar a regla, y con ADR nuevo. **Rechazado:** fallback por brillo (rompe con NIR ambiental; el metadato coincidió 480/480 [M]).

Fixtures: los vectores sintéticos del `selftest` de fuprobe se portan ya; las sesiones grabadas **no** contienen los bytes crudos del UVCM (solo campos parseados), así que `nirlockctl record` añade `meta_raw` en hex y la primera sesión de M1 aporta fixtures reales.

### 2.5 Gate de calidad

Idéntico a `quality_gate()`; primer motivo que falla: `no_face` → `low_score` (< 0,75; piso interno del detector 0,30 para registrar cuasi-aciertos; "cara detectada" en resúmenes = ≥ 0,60) → `small_box` (min(w,h) < 64 px) → `multiple_faces` (segunda cara ≥ 0,75 y ≥ 64 px) → `saturated` (> 5 % de píxeles ≥ 250 en la caja del frame **iluminado**) → `underexposed` (media de la caja < 20) → `pose` (|roll| > 20°, |yaw| > 0,35, pitch fuera de [0,30, 0,85], desde los 5 landmarks en el marco de la cara según `estimate_pose()`). Tamaño de caja medido [M], por sesión: en las sesiones de enrolamiento de E5 (`enroll-a/b`, 2026-09-19) el genuino dio **p5/p50/p95 = 88/95/110 px**; en las corridas de E4/E6 (2026-09-20/22, misma pieza, otra distancia) la cara midió **119–138 px**. El texto "95–140 px ↔ 50–65 cm" de las propuestas era una fusión de ambas y sobreestimaba el rango de enrolamiento; la instrucción de enrolamiento (§7) usa el primer rango. Gate de verificación = el de Fase 0 (`min_score 0,75`, `max_sat 0,05`). Gate de enrolamiento: **el mismo de Fase 0 por defecto** (las plantillas de E5 que justifican el umbral 0,45 se construyeron con 0,75/0,05, y una plantilla v1 debe tener la misma distribución de filas que la validada); el gate estricto `min_score 0,80`, `max_sat 0,03` queda como opción `enroll.strict_gate=true`, apagada, que solo se activa si el replay de §10 demuestra que re-enrolar `enroll-a/b` con él conserva ≥ 90 % de las filas y no mueve el máximo impostor LFW más de 0,01.

### 2.6 Inferencia

- Runtime: **`ort = "=2.0.0-rc.13"`, `default-features = false`, `features = ["std", "load-dynamic", "api-27"]`** (rc.13 [V]: default `api-27`, máximo `api-28`, MSRV 1.88; `download-binaries`/`tls-native`/`copy-dylibs` apagados para que `makepkg` compile sin red). `ort::init_from("/usr/lib/libonnxruntime.so")` explícito; un `dlopen` fallido → estado `Cold`, `unavailable models_unavailable`, reintento en el próximo `prewarm`. `depends=(onnxruntime-cpu>=1.28)`; regla en `docs/COMPAT.md`: `api-XX ≤ minor de onnxruntime-cpu en Arch`. Arch reconstruye `onnxruntime-cpu` en cada bump de protobuf/abseil y `libonnxruntime.so.1` es estable, así que el riesgo de soname es menor que en la historia Howdy/dlib. Rechazados: crate `opencv` (clausura enorme en un daemon de autenticación), binarios de pyke, `tract` (sin benchmark; se anota como plan C si ORT no se puede empaquetar). ADR-0015.
- Modelos (`/usr/share/nirlock/models/`, hashes en `manifest.json`): YuNet `face_detection_yunet_2026may.onnx` (MIT, SHA-256 `ebafce4e…`, cabezas crudas → decode de priors 8/16/32, score `sqrt(cls·obj)`, NMS IoU 0,3, top-k 50 reimplementados en Rust y contrastados con fuprobe, ±0,5 px), AuraFace `auraface_glintr100.onnx` (Apache-2.0, `a7933ea5…`, RGB `(x−127,5)/127,5`, 512-d, L2), SFace `face_recognition_sface_2021dec.onnx` (Apache-2.0, `0ba9fbfa…`, solo sombra). SHA-256 verificado **una vez por vida del proceso** (~0,1–0,2 s con SHA-NI), antes de `commit_from_file`.
- **Contratos de entrada, distintos por embedder** (un port que reutilice el preprocesado de AuraFace para SFace produce puntajes distintos en silencio e invalida la comparación sombra con E5):
  - AuraFace: crop alineado 112x112, orden **RGB**, `float32 (x − 127,5) / 127,5`, NCHW, salida L2-normalizada (como `blobFromImage(aligned, 1/127.5, (112,112), (127.5,127.5,127.5), swapRB=true)` en `phase0/pipeline.cpp` 261).
  - SFace: el mismo crop alineado 112x112, orden **RGB** (OpenCV `FaceRecognizerSF::feature` hace `blobFromImage(aligned, 1, Size(112,112), Scalar(0,0,0), swapRB=true, crop=false)` [V, `modules/objdetect/src/face_recognize.cpp` en 4.x]: **valores crudos 0..255, sin resta de media ni escala**, NCHW float32), salida L2-normalizada por nosotros (`l2_normalise` en `pipeline.cpp` 239). Con el gris replicado a 3 canales el `swapRB` es neutro, pero se declara igual. Puntos de destino de la alineación: `(38.2946, 51.6963) (73.5318, 51.5014) (56.0252, 71.7366) (41.5493, 92.3655) (70.7299, 92.2041)`, idénticos para ambos.
  - Prueba de paridad (§10): para los mismos crops alineados guardados por fuprobe, el `score` SFace del port Rust reproduce el de `fuprobe score` con cos ≥ 0,99 por frame; un fallo aquí bloquea M2.
- Alineación: semejanza Umeyama (sin reflexión) de los 5 landmarks a los puntos ArcFace, warp bilineal propio a 112x112, gris replicado a 3 canales. Paridad con fuprobe: cos ≥ 0,99 por frame (OpenCV interpola en punto fijo de 5 bits; no se promete "bit a bit").
- Carga: al `prewarm` o al primer `verify`, con un forward ficticio por sesión. Un `verify` con modelos cargando **abre la cámara igual** y descarta frames hasta que están (`lit_skipped_loading`, como `--cold-models`). `RunOptions` por petición; `terminate()` en cancelación.

### 2.7 Motor de decisión

Reproduce `cmd_latency` (`phase0/main.cpp` 1316–1405):

- Puntaje del frame = **máximo coseno** sobre la unión de las filas de todas las plantillas habilitadas del usuario (AuraFace, variante `lit`).
- `hit` si ≥ **0,45** (provisional, E5: sobre todo impostor visto — LFW máx 0,400 con 123 filas, NIR máx 0,355 — y 0,26 bajo el mínimo genuino 0,708). ADR-0007.
- Ventana, definida con precisión para reproducir fuprobe (`main.cpp` 1332, 1358–1361, 1398–1403): fuprobe lleva `lit_index`, que incrementa en **cada frame utilizable etiquetado como iluminado** que llega al bucle (incluidos los saltados por `models_loading` y por `no_previous_dark`), y cuenta como aciertos en ventana los `hit` con índice `> lit_index − W`, es decir, el frame actual y los 3 iluminados anteriores. En nirlock, **crea entrada de ventana todo frame iluminado, utilizable y etiquetado que llega al pipeline**, pase o no el gate (un frame que no pasa el gate o queda bajo el umbral es una entrada sin `hit`). **No crean entrada**: los frames descartados por "el último gana" (nunca llegan al pipeline: en fuprobe no existe ese descarte porque procesa en serie, y con 8 búferes y ~17 ms de retraso por frame iluminado el rezago en 4 s es < 1 frame) ni los saltados mientras cargan los modelos (en fuprobe sí incrementan `lit_index`, pero todos preceden al primer frame procesado, así que nunca cambian qué aciertos caen dentro de la ventana; el replay lo comprueba comparando el **`sequence`** del frame que produce K=2, no el `lit_index` bruto). La variante `diff` (apagada) sí crea entrada en `no_previous_dark`, como fuprobe. `W=4`. Un `hit` además **caduca a los 800 ms**, medidos sobre la **marca de llegada del frame** (`arrival`, `CLOCK_MONOTONIC` tomada al `DQBUF`; en replay, la `arrival` grabada), nunca sobre "ahora": así el replay es determinista y con la cadencia nominal (133 ms) el TTL solo actúa cuando el pipeline se rezaga (> 6 frames iluminados en 800 ms es imposible; el TTL cubre pausas por carga de modelos o por bloqueo del hilo de inferencia). fuprobe no tiene TTL; la paridad exige que en `enroll-a/b` el TTL no descarte ningún acierto, y el replay lo comprueba. **K=2 → accept.** K=1 rechazado (inflación por intento +0,025 medida), K=3 rechazado (+133 ms por construcción).
- SFace (≥ 0,55) solo en **modo sombra**, calculado tras actualizar la ventana, y sus puntajes van únicamente a `audit.jsonl` (0600). Retro-reflexión de pupila y "cara en frame oscuro" también solo como cues sombra. Fusión AND con SFace: opción `decision.secondary_embedder="sface"`, apagada.
- Variante `diff` (lit − oscuro anterior): opción, apagada (+160 ms, sin ganancia en interiores [M]).
- Fin de petición por: accept, `budget_ms` (4 000, acotado a [1 000, 4 000] para el carril lock), EOF del par, `cancel` con nonce, tapa cerrada, `PrepareForSleep(true)`, pérdida de cámara, pánico.
- **Clasificación al terminar, sea cual sea la causa** (resuelve el bloqueante de contabilidad): `accept`; `reject no_match` si hubo ≥ 1 frame que pasó el gate y quedó bajo el umbral (aunque la petición terminara por cancelación, tapa o presupuesto); `reject no_face` si ningún frame pasó el gate; `unavailable <razón>` solo si la petición no llegó a puntuar por causas del daemon/cámara; `locked_out`; `cancelled` únicamente con 0 frames puntuados.

### 2.8 Máquinas de estado

Daemon: `Starting` (≤ 600 ms: config, state dir, `welcome` disponible desde el primer instante, `sd_notify(READY)`) → `Cold` → `Warm` (modelos + hashes) → `Verifying` (una petición, cámara abierta) → `Warm` → `Draining` (300 s sin sesión de lock ni peticiones) → exit.

**Sin estado `Hold`.** La propuesta ux-latency-first mantenía la cámara en streaming 400 ms tras cada petición para que un reintento no pagara los 253 ms de arranque en frío; con `verify_min_interval_ms = 1500` y el `Cooldown` de 1 500 ms del wrapper ese estado era inalcanzable, sumaba segundos de cámara al cupo sin petición en vuelo y mantenía el emisor encendido sin que nadie lo hubiera pedido (pregunta 5). Se elimina: la cámara se cierra al terminar cada petición (`Closing`). El reintento del wrapper a los 1 500 ms sigue siendo barato porque el USB no autosuspende hasta 2,6 s después del cierre (~180 ms hasta el primer frame en vez de 253 [M]). Prueba de política (§10): tras `Closing`, un `verify` del mismo uid antes de 1 500 ms → `unavailable rate_limited` con la cámara cerrada; ningún estado del daemon mantiene STREAMON sin petición.

Petición: `Received` → `Authorized` (≤ 5 ms) → `Opening` (≤ 113 ms; tras resume hasta 2 000 ms de espera + 3 × 150 ms, todo dentro de `budget_ms`) → `Streaming` (hasta accept / `budget_ms` / EOF / tapa / suspensión) → `Charging` (persiste contadores con `fsync` **antes** de escribir `result`) → `Closing` (STREAMOFF, cierre; el USB autosuspende 2,6 s después) → `Reported`. Un segundo `verify` mientras `Verifying` → `unavailable busy` inmediato.

### 2.9 Errores

| Fallo | Comportamiento |
|---|---|
| Cámara ausente / puerto distinto / QUERYCAP ≠ uvcvideo / formato cambiado | `unavailable camera_missing|camera_mismatch|camera_format`; journal |
| IR `EBUSY` tras reintentos | `unavailable camera_busy` (sin contar como fallo) |
| Metadato ausente/inconsistente | `unavailable metadata` |
| Modelo con SHA distinto / ORT no carga | `unavailable models_unavailable`; `status.face.reason=model_mismatch` |
| Plantilla ausente / hashes distintos | `unavailable not_enrolled|template_stale` |
| `state.json` corrupto | tratado como **bloqueo duro hasta un SAE** (peor caso), cursor del oráculo = "ahora", journal |
| `boot_id` ilegible | igual: bloqueo duro hasta SAE root; nunca "reinicio = nuevo boot" |
| Pánico | `abort` → EOF a todos los clientes → deny; systemd reinicia (`StartLimitIntervalSec=0`); el fuzzing del parser (M3) evita pánicos por entrada |
| Config inválida | exit 78, sin reintento; el socket queda escuchando y el módulo falla en ≤ 2,5 s |

Ningún fallo produce `accept`. El único camino a `PAM_SUCCESS` es un `result` con `outcome:"accept"`, `nonce` y `user` idénticos a la petición, tras K=2/W=4 sobre frames etiquetados por metadato de la cámara fijada, sin que ninguna regla de política lo haya negado.

### 2.10 Registro y auditoría

- journald (`tracing-journald`), campos `NIRLOCK_EVENT` (`verify.accept|verify.reject|verify.unavailable|lockout.soft|lockout.hard|sae.boot|sae.root|sae.audit|enroll.*|delete.*`), `NIRLOCK_USER`, `NIRLOCK_LANE`, `NIRLOCK_PEER_UID`, `NIRLOCK_MS`, `NIRLOCK_FRAMES`, `NIRLOCK_GATE_REJECTS`, `NIRLOCK_RGB`. **Nunca puntajes ni buckets de similitud** (el journal es legible por `wheel`, al que pertenece uid 1000 [V]), nunca cajas, landmarks, embeddings ni imágenes.
- `/var/lib/nirlock/audit.jsonl` (0600, 5 MB × 3): los mismos eventos con puntaje máximo (2 decimales), sombra SFace y cues; `sudo nirlockctl status --history` lo lee.
- Los volcados de frames solo existen en `nirlockctl record` (root, feature de cargo `debug-dump` apagada en el PKGBUILD, rechazado dentro del servicio). Los coredumps de fuprobe presentes en `/var/lib/systemd/coredump` (bench/ortbench, 2026-09-19/20) deben borrarse con `coredumpctl` (pregunta al usuario, §13).

---

## 3. IPC (resumen; normativo en `design/PROTOCOL.md`)

- Socket `/run/nirlock/sock`, `SOCK_STREAM`, 0666; autorización **solo** por `SO_PEERCRED` (uid, gid, pid) y por el rol declarado en `hello` (`pam` | `lock` | `ctl`). Quickshell `Socket` es un `QLocalSocket` con `write(QString)` + `SplitParser` [V], así que el marco es **NDJSON**: un objeto JSON por línea, `\n`, UTF-8, sin caracteres de control crudos, ≤ 8 KiB. Rechazado el prefijo de longitud: desde QML solo se conoce `String.length` (UTF-16), no bytes.
- Verbos uid N (N = usuario objetivo, con sesión activa en `seat0`): `hello`, `status` (propio, sin edad del SAE), `prewarm`, `lock_session`, `verify`, `cancel` (con nonce), `subscribe`, `ping`. Verbos uid 0 + `client=ctl` en una conexión sin `verify`: `enroll`, `enroll_abort`, `list_templates`, `delete`, `attest`, `reset_lockout`, `set_enabled`, `camera_repin`, `config_reload`.
- Límites: ≤ 8 conexiones por uid ≠ 0 (≤ 64 total), `hello` en 2 s, línea incompleta 1 s, token bucket 20 msg/s (ráfaga 50) por conexión, conexiones ociosas sin suscripción **y sin sesión de lock `locked`** cerradas a los 30 s, una ranura reservada para uid 0 y otra para el uid enrolado. Los eventos solo se entregan a conexiones que enviaron `subscribe`; el wrapper lo envía inmediatamente después de `hello` (§5.3).
- La línea `result` para clientes `pam` se emite con `format!` fijo (no serde) y el escáner C compara el prefijo completo `{"v":1,"t":"result","nonce":"` + 32 hex + `","user":"` + nombre + `","outcome":"`.
- Cancelación = cierre del socket (SIGKILL del hijo por `abort()` → EOF → STREAMOFF en < 70 ms + `RunOptions::terminate`). El `cancel` explícito exige el `nonce`.
- Eventos al cliente `lock` (informativos): `resumed`, `lid_open`, `lid_close`, `availability_changed`, `lockout_changed {until_ms, kind}`, `verify_finished {nonce, outcome, reason, retry_after_ms?}` (permite distinguir `unavailable camera_busy` tras resume de `reject`, y `rate_limited` trae cuándo se libera el cupo), `enroll_progress`. La UX de bloqueo del wrapper se guía por estos eventos, no por `PamResult` (§5.4).

---

## 4. Módulo PAM `pam_nirlock.so`

### 4.1 Superficie
`pam_sm_authenticate` (toda la lógica); `pam_sm_setcred` → `PAM_SUCCESS`. Sin `account/session/password`. Opciones: `socket=/run/nirlock/sock`, `timeout=7000` (ms, 1000–30000), `lane=lock` (v1 solo acepta `lock`). Argumento desconocido → `pam_syslog(LOG_ERR)` + `PAM_SERVICE_ERR`.

### 4.2 Algoritmo
Todas las esperas con `poll()` y plazo absoluto en `CLOCK_MONOTONIC`; `EINTR` → recalcular; sin manejadores de señal, sin hilos, sin `dlopen`, sin NSS, sin `malloc` propio, sin conversación PAM.

1. `pam_get_item(PAM_USER)` (**nunca `pam_get_user`**, que abre la conversación si `PAM_USER` está vacío y reabriría la ruta del bug #977 de Quickshell). Vacío o fuera de `^[a-z_][a-z0-9_-]{0,31}$` → `PAM_USER_UNKNOWN`. `PAM_RHOST` no vacío → `PAM_AUTH_ERR`.
2. Nonce de 16 bytes con `getrandom(2)` (fallo → `PAM_SERVICE_ERR`).
3. `socket(AF_UNIX, SOCK_STREAM|SOCK_CLOEXEC|SOCK_NONBLOCK)`, `connect` (500 ms). `ENOENT/ECONNREFUSED/EACCES`/timeout → `PAM_AUTHINFO_UNAVAIL`.
4. `send(MSG_NOSIGNAL)` de `hello` (≤ 128 B). `EPIPE/ECONNRESET` → `PAM_AUTHINFO_UNAVAIL`.
5. Espera `welcome` ≤ **2 500 ms** (cubre el `Starting` de la activación por socket); sin `welcome` → `PAM_AUTHINFO_UNAVAIL`. Entonces calcula **`budget_ms = deadline − now − 300`**, acotado a **[1 000, 4 000]** (si tras el `welcome` quedan < 1 300 ms → `PAM_AUTHINFO_UNAVAIL` sin enviar `verify`, para no abrir la cámara por una petición que ya no puede esperar el resultado), y envía `verify` (≤ 512 B) con ese presupuesto. Luego lee líneas en un búfer de pila de 2 048 B; línea sin `\n` a 2 048 B → `PAM_SERVICE_ERR`. Ignora `ack`, `progress`, `event`; el primer `result` decide; cualquier `error` → `PAM_SERVICE_ERR`.
6. `result` con `nonce` o `user` distintos (byte a byte) → `PAM_SERVICE_ERR`.
7. Cierra, `pam_syslog(LOG_INFO)` una línea sin puntajes, retorna. Plazo total = `timeout` (7 000 ms por defecto), absoluto desde la entrada a `pam_sm_authenticate`. El daemon garantiza `result` en `budget_ms` + 200 ms desde el `verify` (PROTOCOL §8) y **todo su trabajo previo a la apertura de la cámara** (espera de reaparición tras resume ≤ 2 000 ms, reintentos `EBUSY` 3 × 150 ms) corre dentro de ese presupuesto; el módulo, al restar 300 ms, siempre recibe el `result` antes de su plazo (peor caso: connect 500 + welcome 2 500 + budget 3 700 + 200 = 6 900 < 7 000). El caso "el módulo devuelve `AUTHINFO_UNAVAIL` mientras el daemon aún carga un fallo y escribe a un socket cerrado" desaparece por construcción; en el caso normal (`welcome` en ms) el presupuesto es el máximo, 4 000.

### 4.3 Tabla de códigos de retorno

| `outcome` / condición | PAM | Lane `[success=done maxtries=die default=ignore]` + `required pam_deny` | Quickshell 0.3.1 [H, E8b] |
|---|---|---|---|
| `accept` (nonce+user correctos) | `PAM_SUCCESS` (0) | done → éxito | `completed(Success)` |
| `reject` (`no_match`, `no_face`) | `PAM_AUTH_ERR` (7) | ignore → `pam_deny` → 7 | `completed(Failed)` |
| `locked_out` | `PAM_MAXTRIES` (11) | die → 11 (verificado: `s_die_maxtries_deny` → 11) | `completed(MaxTries)` **o** `completed(Error)`: el enum `MaxTries` existe en los qmltypes, pero el mapeo no es verificable en los archivos instalados y los hechos del sistema indican que solo `PAM_AUTH_ERR` → `Failed` y el resto → `Error`. E8b lo mide; **el wrapper no depende de ello** (§5.4: el bloqueo se aprende por `verify_finished`/`lockout_changed`) |
| `unavailable` (cualquier razón, incl. `stale`, `not_enrolled`, `disabled`, `account_locked`, `lid_closed`, `camera_*`, `metadata`, `models_unavailable`, `rate_limited`, `busy`, `no_session`, `suspending`) | `PAM_AUTHINFO_UNAVAIL` (9) | ignore → `pam_deny` → 7 | `Failed` |
| `cancelled` | `PAM_ABORT` (26) | ignore → deny | (hijo ya muerto normalmente) |
| socket ausente / rechazado / sin `welcome` / EOF / `EPIPE` | `PAM_AUTHINFO_UNAVAIL` | ignore → deny | `Failed` |
| error de protocolo / nonce-user distintos / versión / línea larga / opción inválida | `PAM_SERVICE_ERR` (3) | ignore → deny | `error(TryAuthFailed)` + `completed(Error)` |
| `PAM_RHOST` fijado / `lane ≠ lock` | `PAM_AUTH_ERR` / `PAM_SERVICE_ERR` | deny | `Failed` / `Error` |
| usuario ausente/inválido | `PAM_USER_UNKNOWN` (10) | ignore → deny | `Error` |
| **nunca** | `PAM_IGNORE` (25), `PAM_PERM_DENIED`, `PAM_CRED_*`, `PAM_SUCCESS` sin `accept` | — | — |

`stale` va como `unavailable` (no `MAXTRIES`): el plugin aprende la razón por `status`/`verify_finished`, y no entra en un estado "bloqueado hasta HH:MM" indefinido. Quickshell mapea `PAM_SUCCESS→Success` y `PAM_AUTH_ERR→Failed`; según los hechos verificados del sistema **todo lo demás** (incluido `PAM_MAXTRIES`) llega como `error()` **y** `completed(Error)` (dos señales por fallo: el wrapper usa un solo `onCompleted`). Si E8b muestra que `MaxTries` sí se emite, es un dato, no una dependencia: el wrapper trata `MaxTries` y `Error` igual y decide por los eventos del daemon.

Por qué `default=ignore` + `pam_deny` y no `sufficient`/`required` (pam 1.7.2, `research/prototypes/pamtest`): `sufficient + pam_deny` aplana `MAXTRIES` y `AUTHINFO_UNAVAIL` a `AUTH_ERR`; un lane que incluya `system-auth` tras un resultado ignorado se abre por su `auth optional pam_permit.so` (línea 9 en esta máquina [V]). Nuestro lane nunca incluye `system-auth` y termina en `pam_deny`: `.so` ausente, servicio ausente (`other` → deny) o código ignorado, todos deniegan.

### 4.4 Directorio PAM propio y lanes (verbatim)

**Hecho que manda [V, 2026-09-23]:** Quickshell 0.3.1 arranca todo `PamContext` con `pam_start_confdir(config, user, conv, configDirectory)` y el valor por defecto de `configDirectory` es `/etc/pam.d` (`/usr/bin/quickshell` importa `pam_start_confdir` y **no** `pam_start`, y contiene el literal `/etc/pam.d`; el qmltype expone `configDirectory`). libpam, en la ruta con confdir, busca **solo** `<confdir>/<servicio>` y luego `<confdir>/other`: **no hay fallback al directorio de proveedor `/usr/lib/pam.d`**. Verificado en esta máquina con un shim `LD_PRELOAD` que registra `fopen`: `pam_start_confdir("polkit-1", …, "/etc/pam.d")` abre `/etc/pam.d/polkit-1` (ENOENT) y luego `/etc/pam.d/other`, mientras que `pam_start("polkit-1")` abre `/usr/lib/pam.d/polkit-1`. La versión anterior de este diseño instalaba el lane en `/usr/lib/pam.d/omarchy-lock-face`: el lock screen nunca lo habría cargado (cada ráfaga habría caído a `other` → deny, fail-closed pero inútil) mientras `FaceLane` y `doctor` lo daban por armado. El `[V]` de entonces valía para `pam_start()`, no para el anfitrión real.

**Decisión (ADR-0012, reescrito):** el paquete instala su propio directorio PAM, `/usr/lib/nirlock/pam.d/` (root 0755, archivos 0644, `pacman -Qkk` los verifica), y el `PamContext` del wrapper lo selecciona con `configDirectory: "/usr/lib/nirlock/pam.d"`. Consecuencias: (1) el archivo que el lock screen lee es exactamente el que el paquete instaló; (2) ningún `/etc/pam.d/omarchy-lock-face` ajeno (upstream Howdy, #8336) puede anular ni sustituir nuestro lane, y la reescritura semanal de `/etc/pam.d/omarchy-lock-*` por `omarchy-apply-lock` deja de ser un riesgo; (3) los `include` también se resuelven contra el confdir, así que el lane **no incluye nada** (la línea `account include system-local-login` que copiaba el lane de huella era inerte —Quickshell no llama `pam_acct_mgmt` [V]— y aquí además apuntaría a un archivo inexistente); (4) el servicio se llama `nirlock-lock`, no `omarchy-lock-face`, para que nadie busque el archivo en `/etc/pam.d`. Un plugin uid 1000 malicioso podría cambiar `configDirectory` a un directorio propio: solo afecta a su propio proceso, y ese código ya puede llamar `finishUnlock()` (§1.2). `nirlockctl` (root) usa **la misma llamada** `pam_start_confdir("nirlock-admin", user, conv, "/usr/lib/nirlock/pam.d")` para la atestación: un solo directorio, sin `/etc` que pueda interponerse (con `pam_start()` un `/etc/pam.d/nirlock-admin` ajeno sí tendría prioridad sobre `/usr/lib/pam.d`).

`/usr/lib/nirlock/pam.d/nirlock-lock` (carril de rostro del lock screen):
```
#%PAM-1.0
# nirlock: face lane for the Omarchy lock screen. Read ONLY through
# PamContext.configDirectory = /usr/lib/nirlock/pam.d (pam_start_confdir); /etc/pam.d is
# never consulted for this service. nirlock.lock disarms unless this file is byte-identical
# to the packaged one. No includes: they resolve against this directory. Never add
# system-auth: its "auth optional pam_permit.so" turns an ignored result into success.
auth     [success=done maxtries=die default=ignore]  pam_nirlock.so lane=lock timeout=7000
auth     required                                    pam_deny.so
```

`/usr/lib/nirlock/pam.d/nirlock-admin` (atestación root de `nirlockctl attest|reset-lockout|enroll|delete`, §6.1 b):
```
#%PAM-1.0
# nirlock: root-side password check of the TARGET user before any admin verb.
# pam_unix as root reads /etc/shadow directly (no unix_chkpwd, no audit record).
# No faillock, no system-auth (its "auth optional pam_permit.so" would fail open),
# no nullok (an empty password never attests). success=done / default=die return
# pam_unix's own code (7 on a wrong password); pam_deny is a backstop that must stay last.
auth     [success=done default=die]                  pam_unix.so
auth     required                                    pam_deny.so
```
`nirlockctl` llama `pam_start_confdir` con una conversación que **solo** responde a `PAM_PROMPT_ECHO_OFF` con la contraseña leída del terminal (`getpass`-style, sin eco) y devuelve `PAM_CONV_ERR` ante cualquier otro estilo de mensaje (`ECHO_ON`, `TEXT_INFO`, `ERROR_MSG`): un lane manipulado que intente otra cosa aborta la atestación. Nunca `pam_acct_mgmt`, nunca `pam_setcred`. Códigos esperados en la suite pamtest (§10): contraseña correcta 0; incorrecta 7; usuario inexistente ≠ 0 (7 o 10 según `pam_unix` [H, el arnés fija el valor medido]); archivo ausente → `other` → 7; nombre vacío o fuera de `^[a-z_][a-z0-9_-]{0,31}$` rechazado por `nirlockctl` antes de tocar PAM. `pam_unix` aplica su retardo de fallo (~2 s) y el CLI no lo desactiva.

`/usr/lib/nirlock/pam.d/other` (red de seguridad del confdir; libpam lo consulta si el servicio pedido no existe):
```
#%PAM-1.0
auth     required   pam_deny.so
account  required   pam_deny.so
password required   pam_deny.so
session  required   pam_deny.so
```

`omarchy-apply-lock` solo escribe `/etc/pam.d/omarchy-lock-password` y `-fingerprint` [V] y no toca `/usr/lib/nirlock`. Consecuencia documentada en THREAT-MODEL T21 (sin cambio): una cuenta bloqueada con `passwd -l` sigue pudiendo desbloquear por rostro una sesión viva; el daemon aplica su propia comprobación parcial (§6.3).

### 4.5 En el hijo de Quickshell
`fork()` sin `exec` de un proceso Qt multihilo; solo libpam, nuestro módulo y `pam_deny` corren ahí. `abort()` = `SIGKILL` + `waitpid` bloqueante en el hilo de UI: nuestras esperas están acotadas por `timeout`, y la muerte a mitad de petición es el camino normal de cancelación para el daemon. Nada queda atrás. Build: `cc -shared -fPIC -fvisibility=hidden -O2 -D_FORTIFY_SOURCE=3 -fstack-protector-strong -Wl,-z,relro,-z,now,-z,noexecstack -Wall -Wextra -Werror`, `#pragma GCC poison PAM_IGNORE pam_get_user`, símbolos exportados solo `pam_sm_authenticate`/`pam_sm_setcred`; `nirlock_wire.c` (escritor/escáner de líneas) separado para pruebas unitarias y fuzzing.

---

## 5. Plugin de Omarchy `nirlock.lock`

### 5.1 Layout y manifiesto (verbatim)

`~/.config/omarchy/plugins/nirlock.lock/` — copia (nunca symlink: `omarchy-plugin-validate` los rechaza [V]) desde `/usr/share/nirlock/plugin/lock/`: `manifest.json`, `Service.qml`, `FaceLane.qml`, `README.md`, `LICENSE` (MIT, convención de los plugins de Omarchy).

```json
{
  "schemaVersion": 1,
  "id": "nirlock.lock",
  "name": "Lock Screen + Face (nirlock)",
  "version": "0.1.0",
  "author": "nirlock",
  "license": "MIT",
  "description": "Stock Omarchy lock screen loaded unchanged, plus one IR face-unlock PAM lane (nirlock-lock, from /usr/lib/nirlock/pam.d) served by nirlockd.",
  "omarchy": { "clonedFrom": "omarchy.lock" },
  "kinds": ["service"],
  "keepLoaded": true,
  "entryPoints": { "service": "Service.qml" }
}
```

Verificado en `PluginRegistry.qml` (4.0.4): `stampHostCapabilities()` copia `["authentication"]` a un manifiesto de terceros con `clonedFrom` (líneas 104–116); `setEnabled(true)` añade `{id}` a `plugins[]`, `omarchy.lock` a `disabledPlugins[]` y lo registra en `cloneSourceRestores[]` (548–551); `setEnabled(false)` → `restoreCloneSource` (555) **solo mientras el manifiesto del clon sigue instalado**. `ensureService()` crea los servicios de autenticación con padre `null` en `AuthServiceStore` e inyecta `omarchyPath`/`shell` **después** de `createObject`.

### 5.2 Wrapper `Service.qml` (mínimo, dos Loaders)

```qml
import QtQuick
import Quickshell
import Quickshell.Io

Item {
  id: root
  property var shell: null                 // inyectado por el host tras createObject (facade con alcance)
  property string omarchyPath: ""          // inyectado por el host
  // Resuelto en la declaración, como hace shell.qml: nunca cambia tras la carga.
  readonly property string stockUrl: "file://" + (Quickshell.env("OMARCHY_PATH") || "/usr/share/omarchy") + "/shell/plugins/lock/Service.qml"

  Loader {
    id: stock
    source: root.stockUrl
    asynchronous: false
    onStatusChanged: if (status === Loader.Error) { console.warn("nirlock: stock lock failed to load"); selfDisable.running = true }
  }
  Binding { target: stock.item; property: "shell"; value: root.shell; when: stock.status === Loader.Ready && stock.item && ("shell" in stock.item) }
  Binding { target: stock.item; property: "omarchyPath"; value: root.omarchyPath; when: stock.status === Loader.Ready && stock.item && ("omarchyPath" in stock.item) }

  Loader {
    id: lane
    source: "FaceLane.qml"
    active: stock.status === Loader.Ready
    onStatusChanged: if (status === Loader.Error) console.warn("nirlock: FaceLane failed to load; stock lock unaffected")
    onLoaded: item.stock = stock.item
  }

  // Autocuración: si el lock original no carga, devolver omarchy.lock (restoreCloneSource) sin esperar al doctor.
  Process { id: selfDisable; command: ["omarchy-shell", "shell", "setPluginEnabled", "nirlock.lock", "false"] }
}
```

Un error QML en `FaceLane.qml` solo deja sin rostro; el lock original sigue con su `IpcHandler { target: "lock" }`, así que `omarchy-shell lock lock|status|preview`, `omarchy-system-lock`, `omarchy-system-sleep-lock` y el servicio `omarchy.idle` no cambian. El original corre bajo la *facade* de shell que reciben los plugins de terceros (no el shell de confianza); hoy no la usa, y `doctor` avisa si un `Service.qml` futuro usa `shell.` de formas no listadas en `docs/COMPAT.md`.

### 5.3 `FaceLane.qml` (esquema)

```qml
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.Pam
import Quickshell.Wayland

Item {
  id: lane                                   // id propio del archivo: los ids no cruzan componentes
  property var stock: null
  readonly property string userName: Quickshell.env("USER") || Quickshell.env("LOGNAME")
  readonly property bool contractOk: stock && ("locked" in stock) && ("lockRequested" in stock)
      && ("authenticatingPassword" in stock) && ("failureMessage" in stock) && ("lastEvent" in stock)
      && typeof stock.finishUnlock === "function" && typeof stock.runWake === "function"
  property bool secure: false                // latched de lastEvent secure=true/false
  property bool graceElapsed: false          // 3 s tras secure=true; lo pone el Timer `grace`, no un binding sobre Date.now()
  property bool laneOk: false                // contenido del lane == esperado
  property bool daemonUp: false
  property bool faceAvailable: false         // welcome/status.face.available
  property bool lockedOut: false             // verify_finished locked_out / lockout_changed
  readonly property bool armed: contractOk && stock.locked && secure && graceElapsed && laneOk && daemonUp
      && faceAvailable && !lockedOut && !(stock.faceConfigured === true)   // upstream con carril propio activo → cedemos
  property int burstToken: 0
  property int burstsThisLock: 0
  property string state: "Idle"              // Idle|Armed|Bursting|Cooldown|Blocked|Exhausted
  readonly property string pamDir: "/usr/lib/nirlock/pam.d"
  // Comparación normalizada: se descartan comentarios y líneas vacías y se colapsan los espacios;
  // las dos líneas restantes deben ser exactamente estas, en este orden.
  readonly property var expectedLane: [
    "auth [success=done maxtries=die default=ignore] pam_nirlock.so lane=lock timeout=7000",
    "auth required pam_deny.so" ]

  // Un binding con Date.now() se evalúa una vez y nunca vuelve a evaluarse con el paso del tiempo:
  // la gracia de 3 s es un Timer explícito.
  Timer { id: grace; interval: 3000; onTriggered: lane.graceElapsed = true }
  Connections { target: stock; function onLastEventChanged() {
      if (stock.lastEvent === "secure=true") { lane.secure = true; lane.graceElapsed = false; grace.restart(); lane.prewarm() }
      else if (stock.lastEvent === "secure=false" || stock.lastEvent === "unlocked") { lane.secure = false; grace.stop(); lane.graceElapsed = false } } }
  Connections { target: stock; function onLockedChanged() { if (!stock.locked) lane.onUnlocked(); else lane.connectCtl() } }

  // configDirectory es obligatorio: Quickshell usa pam_start_confdir y por defecto solo mira /etc/pam.d (§4.4).
  PamContext { id: facePam; config: "nirlock-lock"; configDirectory: lane.pamDir; user: lane.userName
    onCompleted: function(result) { lane.onFaceDone(result, lane.burstToken) } }   // un solo handler: Error también llega aquí

  // Se vigila EXACTAMENTE el archivo que el PamContext va a leer. Se arma solo si el texto normalizado coincide.
  FileView { id: laneFile; path: lane.pamDir + "/nirlock-lock"; watchChanges: true; printErrors: false
    onLoaded: lane.checkLane(); onLoadFailed: lane.checkLane(); onFileChanged: reload() }

  Socket { id: ctl; path: "/run/nirlock/sock"
    parser: SplitParser { splitMarker: "\n"; onRead: function(line) { lane.onDaemonLine(line) } }
    onConnectionStateChanged: {
      lane.daemonUp = connected
      if (connected) {                       // hello → subscribe (sin suscripción no llegan eventos y la conexión
        lane.send({v:1,t:"hello",client:"lock",ver:"0.1.0"})   // ociosa se cerraría a los 30 s) → lock_session
        lane.send({v:1,t:"subscribe"})       // events: todos por defecto
        if (stock.locked) lane.send({v:1,t:"lock_session",state:"locked"})
      } } }
  Timer { id: reconnect; interval: 5000; repeat: true; running: lane.contractOk && stock.locked && !ctl.connected
    onTriggered: ctl.connected = true }      // imperativo: un binding no re-dispara tras ENOENT
  function send(o) { ctl.write(JSON.stringify(o) + "\n"); ctl.flush() }

  IdleMonitor { id: activity; enabled: lane.armed; timeout: 1.0; respectInhibitors: false
    onIsIdleChanged: if (!isIdle) { blankGuard.restart(); lane.requestBurst("activity") } }   // cada flanco idle→activo reinicia la guardia (no el flanco a idle: alargaría la guardia más allá del apagado de pantalla)
  Timer { id: presence; interval: 1500; repeat: true; running: lane.armed && !activity.isIdle && lane.state === "Cooldown"
    onTriggered: lane.requestBurst("activity") }   // nivel, no solo flanco: sigue intentando mientras el usuario se mueve
  // blankGuard se (re)arranca en requestBurst() y en cada flanco de IdleMonitor; sin actividad 4 800 ms aborta la
  // ráfaga antes de que el original apague la pantalla (5 000 ms).
  Timer { id: blankGuard; interval: 4800; onTriggered: if (lane.state === "Bursting" && !stock.authenticatingPassword) lane.abortBurst() }
  function requestBurst(trigger) {
    if (!armed || state === "Bursting" || state === "Cooldown" || state === "Blocked" || stock.authenticatingPassword
        || burstsThisLock >= 8) return
    burstToken++; burstsThisLock++; state = "Bursting"; blankGuard.restart()
    if (!facePam.start()) { state = "Cooldown"; cooldown.restart() }
  }
  Timer { id: cooldown; interval: 1500; onTriggered: if (lane.state === "Cooldown") lane.state = "Armed" }
}
```

Contrato con el original (todos presentes en 4.0.4 [V]): `locked`, `lockRequested`, `authenticatingPassword`, `failureMessage`, `lastEvent`, `finishUnlock()`, `runWake()`; opcional `faceConfigured` (PR #8336). Si falta algo → `contractOk=false`, carril inerte, una línea en el journal y una notificación única.

### 5.4 Política de disparo

| Momento | Acción |
|---|---|
| `lockRequested` → true | conectar `ctl`; al conectar: `hello`, **`subscribe`** (sin suscripción el daemon no entrega eventos y cerraría la conexión ociosa a los 30 s), `lock_session locked`. **Sin escaneo.** La conexión `lock` con sesión `locked` queda exenta del cierre por inactividad (PROTOCOL §3), así que un lock de horas conserva la residencia y los eventos sin reconectar |
| `lastEvent == "secure=true"` | `prewarm`; `grace.restart()`; el carril se arma cuando `graceElapsed` pasa a `true` 3 s después (evita "lock → mirar → rozar el trackpad → desbloqueado") |
| Actividad (`IdleMonitor` idle→activo) con `armed` | `blankGuard.restart()`; `requestBurst("activity")` |
| Presencia sostenida (`!isIdle`) tras un `Cooldown` | reintento cada 1 500 ms hasta `burstsThisLock ≥ 8` o `lockedOut` |
| `event resumed` / `event lid_open` | `burstsThisLock` no se toca; se espera `event availability_changed` (cámara reaparecida, ≤ 2 s) y se lanza una ráfaga; `unavailable camera_*` en los 3 s siguientes no cuenta ni enfría |
| `requestBurst` | rechazado si `Bursting`, `Cooldown` (1 500 ms), `Blocked`, `stock.authenticatingPassword`, `burstsThisLock ≥ 8`, tapa cerrada (`status`), o `!armed`; si no, `burstToken++`, `burstsThisLock++`, `blankGuard.restart()`, `facePam.start()`; `false` → `Cooldown` |
| Ráfaga en curso | dura el `budget_ms` del daemon (≤ 4 000 ms); `blankGuard` (4 800 ms sin actividad desde el último flanco o desde el inicio de la ráfaga) la aborta antes del apagado de pantalla del original (5 000 ms), salvo que haya contraseña en vuelo |
| `verify_finished` (evento) | `accept` → nada (el `completed(Success)` hace el trabajo); `reject` → `Cooldown`; `locked_out` → `lockedOut = true`, `Blocked` para este lock, mensaje `Face locked: password`; `unavailable`: `camera_*` tras resume → sin `Cooldown`; `rate_limited` → `Cooldown` hasta `retry_after_ms` (si falta, 30 s); `models_unavailable`/`not_enrolled`/`stale`/`disabled`/`account_locked` → `Blocked` 30 s (mensaje solo para `stale`); resto → `Cooldown` |
| `event lockout_changed` | `until_ms > now` → `lockedOut = true`, `Blocked`; `until_ms == 0` → `lockedOut = false` (bloqueo blando expirado o SAE) |
| `completed(Success)` con token vigente | `if (stock.locked) stock.finishUnlock()` |
| `completed(Failed)` | `Cooldown` (si no llegó ya un `verify_finished` para este token) |
| `completed(MaxTries)` / `completed(Error)` | **el mismo manejo**: si ya llegó `verify_finished` para el token, nada más; si no (daemon caído, lane ausente, `StartFailed`), `Blocked` 30 s (nada de reintentos a 250 ms: cada ráfaga aquí enciende la cámara). El wrapper **no** deduce el bloqueo de `PamResult` (el mapeo de `PAM_MAXTRIES` en Quickshell no está verificado, §4.3); lo deduce de `verify_finished {outcome: locked_out}` y `lockout_changed` |
| `authenticatingPassword` → true | no se aborta el rostro (≤ 4 s; el primero que termine gana) |
| `locked` → false | `facePam.abort()` si `active`; `burstToken++`; `lock_session unlocked`; `state = Idle`; `burstsThisLock = 0`; `grace.stop()`; `graceElapsed = false` |
| `event lid_close` / `PrepareForSleep` | `abortBurst()`; el daemon ya canceló |

Todo `completed` cuyo token no sea el vigente se ignora (un `Success` encolado de un lock anterior nunca llama `finishUnlock()`). `abort()` no emite `completed`: el estado se fija explícitamente. El presupuesto real vive en el daemon (§6); los límites del QML son cortesía. Cupos y ráfagas: 8 ráfagas × ≤ 4 000 ms = 32 s de cámara por lock, dentro del cupo de **36 s / 300 s** del daemon (§6.3); si aun así llega `rate_limited` (p. ej. dos locks seguidos), el wrapper enfría hasta `retry_after_ms` y no lo cuenta como fallo.

### 5.5 Retroalimentación

`LockView` vive dentro del `WlSessionLockSurface` del original y no expone su árbol; una capa Overlay **no se ve** sobre `ext-session-lock` en Hyprland 0.56 sin la regla `above_lock` (que Omarchy no configura) [V]. v1 usa **solo** `stock.failureMessage` (propiedad raíz escribible; `LockView` la pinta en cursiva, color de error, borde rojo, elide ≥ ~22 caracteres, y la limpia al teclear). Solo estados terminales, solo si `failureMessage === ""` y `!authenticatingPassword`, y llamando `stock.runWake()` para que se vea:

| Estado | Texto (≤ 22 caracteres) |
|---|---|
| `locked_out` (por `verify_finished`/`lockout_changed`) | `Face locked: password` |
| `stale` | `Password to re-enable` |
| `no_match` × 3 en un lock | `Face not recognised` |
| `camera_busy` | `Camera in use` |
| `no_face`, `unavailable` transitorios, escaneando | nada |

Indicador gráfico (`faceIndicator` con `above_lock` o el de upstream #8336): v1.1, pregunta 4 al usuario (y condicionado a lo que M1 escriba sobre el LED, pregunta 5).

### 5.6 Carreras y abortos

| Evento | Carril contraseña | Carril huella | Carril rostro |
|---|---|---|---|
| Rostro `Success` | abortado por `finishUnlock()` → `resetAuthenticationState()` | abortado igual | hecho |
| Contraseña `Success` | hecho | abortado por el original | `locked` → false → `abort()`; `completed` tardío ignorado por token |
| Contraseña enviada durante una ráfaga | corre | — | sigue; el primer `Success` gana; `finishUnlock()` es idempotente (`if (!locked && !lockRequested) return`) |
| Ráfaga pedida con `authenticatingPassword` | — | — | rechazada |
| Apagado de pantalla | — | armada (original) | abortada por `blankGuard` |
| Tapa cerrada / suspensión | — | — | abortada; el daemon cancela y cierra la cámara |
| `Error` (`StartFailed`: lane ausente, `configDirectory` ilegible) | — | — | `Blocked` 30 s; `FileView` re-arma cuando el lane reaparece |

### 5.7 Coexistencia y retiro con upstream

Estado el 2026-09-23 [V]: PR #6863 cerrado el 2026-09-21, consolidado en **PR #8336** (abierto, Howdy: `faceConfigured` exige `/etc/pam.d/omarchy-lock-face` **y** un modelo de Howdy en `/etc/howdy/models/$USER.dat` o `/var/lib/howdy/…`; escanea al bloquear y en `runWake()`; su setup escribe `auth required pam_howdy.so` en el mismo archivo). Regla del wrapper: ceder **solo si `stock.faceConfigured === true`** (carril upstream realmente activo); con nirlock enrolado y sin modelos Howdy, el carril upstream queda dormido y el nuestro sigue. Si el usuario instala Howdy además, `doctor` detecta doble escaneo y el wrapper cede. Un `/etc/pam.d/omarchy-lock-face` escrito por un setup upstream **no afecta** a nuestro lane (vive en `/usr/lib/nirlock/pam.d/nirlock-lock` y se lee por `configDirectory`, §4.4); `doctor` lo menciona solo como información ("carril upstream presente; activo únicamente si `faceConfigured`"). Nunca escribimos ni borramos ese archivo.

Retiro completo: `nirlock-remove` ejecuta `omarchy-plugin-disable nirlock.lock` **antes** de tocar el directorio (si se borra primero, `restoreCloneSource` nunca corre y la máquina queda **sin lock screen**: `omarchy-system-lock` descarta el error y `omarchy-sleep-lock` suspende sin bloquear tras 12 s [V]). Camino sin shell: editar `shell.json` (quitar `nirlock.lock` de `plugins[]`, `omarchy.lock` de `disabledPlugins[]` **y** `nirlock.lock` de `cloneSourceRestores[]`) con escritura atómica; el shell lo aplica en vivo (FileView con `watchChanges`). Ambos caminos se niegan mientras `omarchy-shell lock isLocked` sea `true` (destruir el wrapper deja caer el cliente `ext-session-lock`).

### 5.8 Fallos y recuperación

- **Wrapper que no instancia** (cambio de Quickshell): `ensureService` avisa y no hay servicio de lock → clase "no se puede bloquear / suspende expuesto". Mitigación en capas: (1) `Service.qml` mínimo, solo `QtQuick`/`Quickshell`/`Quickshell.Io`, lo frágil en `FaceLane.qml` detrás de su propio `Loader`; (2) autocuración `selfDisable` si el original no carga; (3) hook `post-update.d/nirlock-doctor`: comprobación **estática** del nuevo `Service.qml` original (nombres del contrato, presencia o no de un carril `omarchy-lock-face` upstream) y `systemd-run --user --on-active=60s nirlockctl doctor --repair-plugin` (porque `omarchy-update` corre los hooks **antes** de `omarchy-update-restart` [V], y el wrapper es `keepLoaded`: el proceso viejo no ve el archivo nuevo); (4) hook `post-boot.d` con sondeo de `omarchy-shell lock status` hasta 30 s (como `omarchy-restart-shell`); (5) `--repair-plugin` solo actúa ante `Target not found.` para `lock` con el shell respondiendo a `ping`, nunca ante `not running`/`not ready`, nunca con la sesión bloqueada, y **nunca llama `omarchy-refresh-shell`** (restablece `shell.json` a los valores por defecto y borra los widgets del usuario [V]); (6) desde TTY: `nirlockctl plugin disable --offline` (jq sobre las tres claves), luego `omarchy-restart-shell` si hace falta. `SECURITY.md` dice que `pacman -Syu` fuera de `omarchy-update` salta el hook (queda el de arranque).
- Original cargado pero nombres cambiados: carril inerte, lock intacto.
- Daemon ausente: `Socket` no conecta → carril nunca arranca; nada visible; reconexión cada 5 s.
- Lane ausente o distinto en `/usr/lib/nirlock/pam.d/nirlock-lock`: desarmado (PAM caería a `/usr/lib/nirlock/pam.d/other` → deny de todos modos; y si ese también faltara, libpam sin configuración deniega).
- Actualización del propio plugin: `keepLoaded` ⇒ el archivo nuevo no se carga hasta reiniciar el shell; `nirlock-setup --upgrade` termina con `omarchy-restart-shell` (si no está bloqueado) o lo imprime; `doctor` compara la versión en disco con la que reporta el wrapper por `hello.ver`.
- Issue #8762 (primer lock tras autologin sin conversación PAM): se ejercita en E9; si afecta, la primera ráfaga se retrasa hasta ver `secure=true` (ya es la regla).
- E9 (spike sin root) usa `PamContext.configDirectory` apuntando a `~/.local/share/nirlock-test/pam.d` con una copia de los tres archivos de §4.4 (sin includes que resolver); el plugin distribuido define **exactamente** `configDirectory: "/usr/lib/nirlock/pam.d"` y `doctor` comprueba ese valor (no su ausencia: sin él, Quickshell leería `/etc/pam.d/nirlock-lock`, inexistente → `other` → deny, y el rostro nunca funcionaría, §4.4).

---

## 6. Política (todo se aplica en el daemon)

### 6.1 Definiciones
- **Fallo de reconocimiento** = petición con ≥ 1 frame que pasó el gate puntuado bajo el umbral y sin `accept`, **sea cual sea su terminación** (presupuesto, EOF, tapa, suspensión, cancel). `no_face`, `unavailable` y `cancelled` con 0 frames puntuados no son fallos.
- **Evento de autenticación fuerte (SAE)** = (a) **boot** con `boot_id` nuevo, solo si `freshness.boot_is_strong_auth = true` (el setup lo escribe tras `cryptsetup luksDump` de la raíz: `true` solo si el único keyslot es de passphrase y no hay tokens tpm2/fido2/pkcs11; `systemd-cryptenroll` está instalado aquí [V]); (b) **atestación root**: `sudo nirlockctl attest|reset-lockout|enroll|delete`, que además verifica la contraseña del usuario objetivo con su propio `pam_authenticate` como root (servicio `nirlock-admin` en `/usr/lib/nirlock/pam.d`, texto verbatim y conversación en §4.4) para ignorar la caché de sudo; (c) **oráculo de auditoría** (opcional, consentido en el setup): registro con `_TRANSPORT=audit`, `_AUDIT_TYPE=1100`, `_AUDIT_FIELD_OP=PAM:unix_chkpwd`, `_AUDIT_FIELD_ACCT=<user>`, `_AUDIT_FIELD_UID=<uid>`, `_AUDIT_FIELD_RES=success`, con `_BOOT_ID` = boot actual y `__MONOTONIC_TIMESTAMP` estrictamente posterior al último fallo **y** a una marca de agua fijada a "ahora" siempre que el cursor sea desconocido (arranque, `state.json` corrupto). **No se filtra por `_UID`**: journald no adjunta `_UID` a los registros `_TRANSPORT=audit` (proceden del kernel, no de un socket de cliente); el uid del proceso auditado va en `_AUDIT_FIELD_UID` (y `_AUDIT_LOGINUID`); un filtro `_UID=<uid>` no coincidiría nunca. Los nombres exactos de campo se confirman en M5 sobre un registro real y quedan en `docs/HARDENING.md`. (d) Un `accept` de rostro **no** es SAE.
- Relojes: temporizadores en `CLOCK_BOOTTIME`; el estado persistido guarda `boot_id` + offsets; `boot_id` nuevo → ver matriz.

Por qué el oráculo es sólido [V]: `pam_unix` en el hijo uid 1000 delega en el setuid `unix_chkpwd`, que llama `audit_log_acct_message(AUDIT_USER_AUTH, …, res=success)` también en el éxito cuando `ruid != 0`; emitir `AUDIT_USER_*` exige `CAP_AUDIT_WRITE`, y journald marca los registros del kernel con `_TRANSPORT=audit` (campo de confianza). Hoy `systemd-journald-audit.socket` está deshabilitado [V]; el setup ofrece `systemctl enable --now systemd-journald-audit.socket`. Que el socket solo baste depende de `Audit=` en `journald.conf` (por defecto `yes` en systemd ≥ 245 [H]: journald activa la auditoría del kernel al arrancar con el socket presente; si M5 muestra que no basta, el setup escribe además `/etc/systemd/journald.conf.d/nirlock-audit.conf` con `[Journal]\nAudit=yes` y reinicia journald con consentimiento). `nirlockctl doctor --oracle` pide un desbloqueo real por contraseña y comprueba que produjo un registro `USER_AUTH … unix_chkpwd … res=success` con los campos de arriba dentro del boot actual; sin ese registro, el doctor marca el oráculo como "no operativo" y la frescura cae a boot/root. Si el usuario declina, `freshness.audit_oracle=false`.

### 6.2 Contadores (`/var/lib/nirlock/state.json`, 0600, escritura atómica + `fsync`)

| Contador | Incrementa | Reinicia |
|---|---|---|
| `consecutive_failures` | fallo de reconocimiento (cargado al primer frame puntuado bajo el umbral, antes del `result`; se descarga si la misma petición termina en `accept`) | `accept`; SAE. **No** lo reinicia la expiración de un bloqueo blando |
| `failures_since_sae` | fallo de reconocimiento | `accept`; SAE (ADR-0009, revisado: un `accept` real ya es el objetivo de cualquier atacante, así que no reiniciar aquí solo castigaba al usuario genuino; antes, 15 `no_match` honestos repartidos en días —lentes, luz de día— llevaban a un bloqueo duro sin oráculo que solo un reinicio o `reset-lockout` levantaba) |
| `soft_lockouts_since_sae` | entrada en bloqueo blando | `accept`; SAE (mismo argumento: el nivel de duplicación no debe crecer entre desbloqueos genuinos) |
| `scored_frames_window` | cada frame puntuado (ventana deslizante 1 h) | tiempo |
| `camera_seconds_window` | segundos de streaming (ventana 300 s, por uid) | tiempo |
| `verifies_this_lock` | cada `verify` del carril lock | `lock_session locked` del uid (forjable → solo alimenta el presupuesto por lock) |
| `last_sae {kind, boottime}` / cursor del oráculo | SAE | — |

### 6.3 Reglas (`/etc/nirlock/config.toml`, valores por defecto)

```toml
[decision]  embedder = "auraface"  threshold = 0.45  k = 2  window = 4  hit_ttl_ms = 800  budget_ms = 4000
[policy]
soft_lockout_after = 5            # consecutive_failures → 60 s × 2^(n-1), tope 900 s
hard_lockout_after = 15           # failures_since_sae → hasta un SAE
max_scored_frames_per_hour = 600  # → unavailable rate_limited (acota el material puntuado por hora)
camera_seconds_per_5min = 36      # ≥ verifies_per_lock × budget_ms: 8 × 4 s = 32 s caben en un lock
verify_min_interval_ms = 1500
verifies_per_10min = 30
verifies_per_lock = 8
prewarm_min_interval_s = 30
[freshness]
max_hours = 24                    # el setup escribe 72 si el oráculo se declina (pregunta 3)
boot_is_strong_auth = false       # el setup lo pone a true solo tras comprobar el LUKS
audit_oracle = false              # el setup lo pone a true si el usuario habilita el socket de auditoría
```

- **Bloqueo blando**: `consecutive_failures ≥ 5` → `locked_out` 60 s × 2^(soft_lockouts_since_sae − 1), tope 900 s. Al expirar **no** se reinicia el contador: el siguiente fallo vuelve a bloquear con el doble. Un atacante paciente no obtiene 100 intentos/día: entre dos SAE hay a lo sumo 15 fallos **sin un `accept` por medio** (un `accept` reinicia los tres contadores de fallo; un atacante que lo consigue ya ha entrado, así que la cota no se debilita frente a él; sí desaparece el bloqueo duro por acumulación honesta entre días).
- **Bloqueo duro**: `failures_since_sae ≥ 15` → hasta un SAE (contraseña en el lock con oráculo activo; si no, `sudo nirlockctl reset-lockout` o un arranque con LUKS). Consecuencia UX que el README y el setup dicen en voz alta: sin oráculo, si el rostro falla 15 veces seguidas sin ningún desbloqueo por rostro entre medias, solo un reinicio o `sudo nirlockctl reset-lockout` lo levanta.
- **Frescura**: `verify` solo si `now − last_sae ≤ max_hours`; si no, `unavailable stale`. Nunca > 7 días.
- **Cupos** (no cuentan como fallos, para no dejar que un proceso uid 1000 provoque bloqueos): 1 `verify` / 1 500 ms, ≤ 30 / 10 min, ≤ 8 por lock, ≤ 36 s de cámara / 300 s, ≤ 600 frames puntuados / h, `prewarm` ≤ 1 / 30 s, una petición en vuelo. `unavailable rate_limited` lleva `retry_after_ms` en el evento `verify_finished` (no en el `result` de formato fijo). La auto-verificación del enrolamiento (§7) no consume ni el cupo de frames puntuados ni el de cámara del usuario ni toca contadores de fallo: corre como fase interna de `enroll` (uid 0).
- **Local/remoto**: `lane == lock`, `rhost` vacío (módulo y daemon), y el uid del par debe tener una sesión logind `Active=true`, `Seat=seat0`, clase `user` (`ListSessions` por zbus). No liga el PID a la sesión (`GetSessionByPID` falla con UWSM [V]); sshd está deshabilitado. Limitación documentada.
- **Tapa**: `LidClosed` de logind → `unavailable lid_closed`.
- **Cuenta bloqueada**: `verify` rechazado con `unavailable account_locked` si el shell del usuario en `passwd` es `nologin`/`false` o `enabled.json` del usuario dice `false` (`unavailable disabled`) (señal parcial; ver THREAT-MODEL T21).
- **Matriz de reinicios**: `accept` → `consecutive_failures`, `failures_since_sae`, `soft_lockouts_since_sae` (no la frescura: `last_sae` no cambia). SAE → todos los contadores de fallo + frescura + nivel de bloqueo. `boot_id` nuevo con `boot_is_strong_auth=true` → equivale a SAE; con `false` → **nada** se reinicia y `last_sae` se carga del disco. Pistas uid 1000 (`lock_session`) → solo `verifies_this_lock`. Tiempo → solo cupos y expiración de bloqueos blandos. Reinicio del daemon → **nada** (invariante probado: `kill -9` durante un bloqueo → sigue bloqueado).

---

## 7. Enrolamiento

`sudo nirlockctl enroll [--label glasses]` (root + verificación PAM `nirlock-admin` de la contraseña del usuario objetivo; el rostro nunca autoriza cambios de enrolamiento).

1. Preflight (`status`): cámara presente y única (`camera_ambiguous` si hay dos candidatos), modelos verificados, tapa abierta, RGB libre o no (aviso), `templates < 4`.
2. Instrucciones + cuenta atrás: "a la distancia habitual de trabajo (cara de ~90–110 px de ancho en el encuadre, lo que midieron las sesiones de enrolamiento de Fase 0), pieza iluminada, mira a la cámara". El CLI muestra el ancho de caja en vivo (paso 3), así que la distancia se guía por el número, no por centímetros.
3. El daemon emite `event enroll_progress` (~7,5/s) y el CLI dibuja una línea: `front  [■■■■■■□□□□] 6/12   face 118 px   exposure ok   pose ok` o el motivo traducido (`too close (saturated)`, `move closer (face 58 px)`, `face the camera (yaw)`, `only one person in view`, `too dark`). Fases: `front` (12 frames), `slightly left`, `slightly right`, `slightly up`, `slightly down` (12 cada una, buckets de pose desde los landmarks), segundo `front` un poco más lejos (caja 85–95 px). Gate de enrolamiento = el de Fase 0 (`min_score 0,75`, `max_sat 0,05`, §2.5; el estricto solo con `enroll.strict_gate=true` tras el replay de §10). Se guardan **todas** las filas aceptadas (AuraFace `lit` y `diff`, SFace `lit` y `diff`), sin promediar. Objetivo 60–72 filas; tope 150 por plantilla, 400 por usuario (E5: pasar de 54 a 123 filas subió el máximo impostor LFW 0,392 → 0,400; el tope acota esa deriva). Mínimo 30 filas en ≥ 3 fases o no se escribe nada.
4. Auto-verificación, **fase interna de `enroll` en el daemon** (no un `verify` por el socket: la matriz de autoridad de PROTOCOL §4 prohíbe `verify` a `ctl`, y los verbos de administración exigen una conexión que nunca lo haya enviado). Tras reunir las filas y **antes** de escribir la plantilla en disco, el daemon cierra y reabre la cámara (misma secuencia de §2.3), emite `event enroll_progress` con `phase: "verify"` y corre el motor de §2.7 con presupuesto 5 000 ms contra la plantilla candidata **sola** (en memoria). Resultado en `enroll_done {template, rows, per_embedder, verified: bool, verify_ms}`; con `verified=false` la plantilla no se escribe, se devuelve `error enroll_verify_failed` y el CLI ofrece repetir. Esta fase **no toca** `consecutive_failures`, `failures_since_sae`, `verifies_this_lock`, el cupo de frames puntuados por hora ni el cupo de cámara del usuario (es uid 0 y sanidad, no una petición de autenticación; sí queda en `audit.jsonl` como `enroll.verify` con su puntaje máximo). Sanidad, no medida de FRR.
5. Resumen (filas por fase, rechazos del gate, rango de caja, RGB concurrente sí/no, `verified` y `verify_ms`).

Sin previsualización de imagen en v1 (los frames no cruzan el socket). `--debug-dump` solo con la feature de cargo `debug-dump`, apagada en el paquete.

Almacenamiento (FUEMB1, compatible con `fuprobe far/score`):
```
/var/lib/nirlock/                        0700 nirlock
  state.json  audit.jsonl
  users/rodrigo/
    enabled.json
    <uuid>/manifest.json                 {label, created, user, uid, rows, phases, gate, camera{usb_sysfs,bcdDevice,kernel}, models{yunet_sha256,auraface_sha256,sface_sha256}, rgb_concurrent, nirlock_version}
    <uuid>/auraface_lit.emb  auraface_diff.emb  sface_lit.emb  sface_diff.emb   ("FUEMB1\0\0", u32 dim, u32 count, float32 LE)
    <uuid>/src.tsv                       fase + índice por fila (sin imágenes)
```
Multi-plantilla: ≤ 4 por usuario, todas las habilitadas se agrupan para el MAX; `sudo nirlockctl template list|enable|disable|rename|delete`. Cambio de modelo: plantilla con hashes distintos no se carga; si ninguna carga → `template_stale`, `doctor` y el hook avisan "vuelve a enrolar". Nunca conversión ni plantillas adaptativas. Borrado: sobreescritura con ceros + `fsync` + `unlink` (best effort en btrfs, documentado) + limpieza de contadores. `sudo nirlockctl export --fuemb` copia con 0600 para evaluar con fuprobe. Sin cifrado ni TPM en v1 (ADR-0010).

---

## 8. Seguridad (resumen; completo en `design/THREAT-MODEL.md`)

Una sola frase vale como invariante: **no existe camino a `PAM_SUCCESS` que no pase por un `accept` del daemon**, y el daemon no acepta sin K=2/W=4 sobre frames etiquetados por el firmware de la cámara fijada, con todos los cupos y bloqueos satisfechos. Todo lo demás (daemon caído, EOF, cámara ocupada, modelo o plantilla inválidos, lane o módulo ausentes, error de protocolo, pánico, cupo excedido) termina en `pam_deny`.

Residuales que el README debe decir en voz alta: foto impresa sin probar (por eso solo lock screen y Clase 2); ningún cue de atención (se puede desbloquear el portátil delante del dueño dormido o distraído, y un tercero al teclado con el dueño a ≤ 80 cm detrás); código del mismo uid ya puede saltarse el lock screen de Omarchy; cualquier proceso del usuario puede pedir un `verify` en cualquier momento y usar la cámara IR + plantilla como oráculo de presencia (sin puntajes) dentro de los cupos.

---

## 9. Empaquetado

### 9.1 Repositorio (`nirlock/`, MIT)
```
Cargo.toml                       workspace; panic="abort"; lto="thin"; -C relro,now
crates/nirlock-wire/             tipos + codec NDJSON (daemon, ctl; fuzz target)
crates/nirlock-cam/              sysfs, V4L2 mmap (bindings de `v4l2-sys-mit` + UVCM/UVCIOC locales), parser UVCM, emparejado, palanca RGB
crates/nirlock-vision/           YuNet decode/NMS, pose, gate, Umeyama, sesiones ORT, FUEMB1, coseno
crates/nirlockd/                 daemon: ipc, política, estado, oráculo (sd-journal FFI), logind (zbus), motor de petición
crates/nirlockctl/               CLI: status, doctor, enroll, template, attest, reset-lockout, record, bench, export, plugin, setup helpers
pam/                             pam_nirlock.c, nirlock_wire.c/.h, Makefile, tests/ (arnés de research/prototypes/pamtest + daemon falso)
plugin/lock/                     manifest.json, Service.qml, FaceLane.qml, README.md, LICENSE
hw/3277-0055.toml
pam.d/nirlock-lock  pam.d/nirlock-admin  pam.d/other     → /usr/lib/nirlock/pam.d/ (confdir propio, §4.4)
systemd/nirlockd.service  nirlockd.socket  sysusers.d/  tmpfiles.d/
config/config.toml.example
scripts/nirlock-setup  nirlock-remove  hooks/nirlock-doctor.hook
models/manifest.json             nombres, tamaños, SHA-256, licencias, URLs con commit (sin pesos en git)
tests/                           replays (datos fuera de git), lanes PAM, latency CI
packaging/aur/nirlock/PKGBUILD   packaging/aur/nirlock-models/PKGBUILD   (pkgbase separado)
docs/DESIGN.md SECURITY.md THREATS.md HARDENING.md COMPAT.md BENCH.md HARDWARE.md PRIOR-ART.md
```

### 9.2 Build
Rust stable vía `rustup` (`mise use rust@stable`, `rust-toolchain.toml`), `cargo build --release --locked`; `ort` con `load-dynamic` (sin descargas); `make -C pam`; QML sin build (`omarchy-plugin-validate` + `/usr/lib/qt6/bin/qmllint -I /usr/lib/qt6/qml` solo para sintaxis). Sin cmake ni meson. Pruebas: `size_of::<v4l2_buffer>() == 88` y números de ioctl contra un selftest de fuprobe.

### 9.3 Modelos
Paquete `nirlock-models` con **pkgbase propio** (versión `2024.08.26`; así una actualización del daemon no rebaja 260 MB): `source=` con commit fijado — AuraFace `https://huggingface.co/fal/AuraFace-v1/resolve/af6d057c9b0ec4071d4c49c80e3539258798b609/glintr100.onnx` (solo ese archivo; el repo también trae pesos SCRFD no comerciales), YuNet y SFace desde `https://github.com/opencv/opencv_zoo/raw/<commit>/…` (**nunca** `raw.githubusercontent.com`, que sirve el puntero LFS de 131 bytes [V]) — y un mirror en una release de GitHub del proyecto. `sha256sums` = los de `models/` aquí. `makepkg` verifica al construir; el daemon al cargar. Instala en `/usr/share/nirlock/models/` con `manifest.json` y `LICENSES/`.

### 9.4 PKGBUILD `nirlock`
`depends=(pam 'onnxruntime-cpu>=1.28' 'nirlock-models>=2024.08.26' systemd jq)`, `optdepends=('quickshell: Omarchy lock wrapper' 'audit: password-attested lockout reset (audit oracle)')`, `makedepends=(rust cargo)`, `backup=(etc/nirlock/config.toml)` (los lanes **no** van en `backup=`: viven bajo `/usr/lib/nirlock/pam.d`, propiedad del paquete). Archivos: `/usr/lib/nirlock/nirlockd`, `/usr/bin/nirlockctl`, `/usr/bin/nirlock-setup`, `/usr/bin/nirlock-remove`, `/usr/lib/security/pam_nirlock.so`, `/usr/lib/nirlock/pam.d/{nirlock-lock,nirlock-admin,other}`, unidades, sysusers/tmpfiles, `/usr/share/nirlock/{plugin,hw,hooks}`, `/etc/nirlock/config.toml`. `install`: `post_install` → `systemd-sysusers`, `systemd-tmpfiles --create`, aviso "run nirlock-setup"; `pre_remove` → `systemctl disable --now nirlockd.socket nirlockd.service` e imprime `nirlock plugin disable --offline` para el usuario registrado en el setup. Ningún archivo `/usr/bin/omarchy-*` (todos son del paquete `omarchy` [V]; una colisión futura rompería `omarchy-update`).

### 9.5 `nirlock-setup` (forma de `omarchy-setup-security-fingerprint`, `# omarchy:requires-sudo=true`)
1. `nirlockctl doctor --hw` (cámara 3277:0055 fija y única, `UVCM` en kernel ≥ 6.17, metadato alternando en una sonda de 2 s — el único uso de cámara del setup).
2. `sudo systemctl enable --now nirlockd.socket`.
3. Frescura: `cryptsetup luksDump` de la raíz → `boot_is_strong_auth`; ofrecer el oráculo (`systemd-journald-audit.socket`) con explicación → `audit_oracle`, `max_hours` 24/72.
4. `sudo nirlockctl enroll` (incluye verificación).
5. Plugin: staging en `~/.config/omarchy/plugins/.nirlock.XXXXXX`, `omarchy-plugin-validate`, `mv` a `nirlock.lock`, `omarchy-shell shell rescanPlugins`, sondeo de `omarchy-plugin-list --json` (≤ 2 s, como `omarchy-plugin-clone`), rechazo si `omarchy-shell lock isLocked` es `true`, `omarchy-plugin-enable nirlock.lock`, sondeo de `omarchy-shell lock status` con `passwordPam:true` (≤ 30 s), si no → rollback. Instalar los hooks `post-update.d` y `post-boot.d`.
6. Imprime: cómo desactivar desde TTY, que la contraseña siempre funciona, qué no protege.

`nirlock-remove`: orden inverso; `omarchy-plugin-disable nirlock.lock` **antes** de borrar el directorio; se niega con la sesión bloqueada; `sudo nirlockctl delete --all`; `systemctl disable --now nirlockd.socket`; pregunta si dejar el socket de auditoría; `omarchy-pkg-drop nirlock nirlock-models`.

### 9.6 `nirlockctl doctor`
Socket y `welcome`; hashes de modelos y plantillas; versión de `libonnxruntime` vs rango probado; los tres archivos de `/usr/lib/nirlock/pam.d/` byte-idénticos a los del paquete (`pacman -Qkk nirlock`) — el doctor mira **ese** directorio, el que el `PamContext` lee, nunca `/usr/lib/pam.d`; información (no aviso) si existe `/etc/pam.d/omarchy-lock-face` (carril upstream, inactivo salvo `faceConfigured`); `omarchy-shell lock status` con `passwordPam:true` y `lastEvent`; wrapper habilitado y cargado (`hello.ver` por el socket vs versión en disco); contrato del `Service.qml` original (nombres, usos de `shell.`), lista de hashes solo informativa (journal, no notificación: el paquete cambia semanalmente); `faceConfigured` upstream activo → doble escaneo; **`configDirectory` del `PamContext` del plugin instalado == `/usr/lib/nirlock/pam.d`** (la comprobación está invertida respecto al diseño anterior: la ausencia es un error, §4.4); oráculo vivo (`--oracle`: pide un desbloqueo real y busca el registro `USER_AUTH` con los campos de §6.1); coredumps de nirlockd ausentes. `--repair-plugin` con las restricciones de §5.8.

---

## 10. Pruebas

- **Unitarias** (`cargo test`, `make -C pam test`, sin cámara ni modelos): `parse_uvcm` (vectores sintéticos de fuprobe + `meta_raw` de M1); descubrimiento sobre un sysfs falso (VID malo, `removable`, driver, ruta, dos dispositivos); `S_FMT` con cambio silencioso (ioctl simulado); decode/NMS de YuNet vs tensores guardados de fuprobe (IoU ≥ 0,98, landmarks ≤ 0,5 px); `estimate_pose`/gate incluido el caso frontal inclinado; Umeyama vs OpenCV (≤ 1e-4); FUEMB1; ventana K/W con caducidad sobre `arrival` y "el último gana" (entradas: todo iluminado utilizable que llega al pipeline; sin entrada los descartados por latest-wins ni los saltados por carga, §2.7); emparejado vídeo↔meta con búferes de meta retrasados hasta 25 ms (no producen "sin etiqueta") y con metadatos tardíos para secuencias ya podadas (se descartan); regla de "misma etiqueta" solo sobre `sequence` consecutivos (un salto de secuencia no la dispara); codec NDJSON (8 KiB, control chars, campos desconocidos, orden fijo del `result`); **política como máquina de estados pura**: `failures_since_sae` y `soft_lockouts_since_sae` se limpian solo con SAE o `accept`, un bloqueo blando expirado no reinicia el contador, `kill -9` no cambia nada, `boot_id` ilegible = bloqueo duro, cara equivocada + tapa a 2,5 s = fallo, cara equivocada + EOF = fallo, `state.json` corrupto = bloqueo duro + marca de agua del oráculo, **sin `Hold`**: tras `Closing` la cámara está cerrada y un segundo `verify` del mismo uid antes de 1 500 ms → `unavailable rate_limited` con `retry_after_ms`, la fase `verify` de `enroll` no mueve ningún contador ni cupo; **ciclo de vida de la conexión `lock`**: un cliente `lock` con `subscribe` + `lock_session locked` que calla 60 s **conserva** la conexión y la residencia y **recibe** `verify_finished` de una petición `pam` posterior; el mismo cliente sin `subscribe` se cierra a los 30 s y su sesión de lock caduca 300 s después; `cargo-fuzz` sobre el parser NDJSON y sobre `nirlock_wire.c` (vía `cc`).
- **Replay** (`nirlockd --camera replay:<sesión>`, un `FrameSource` con implementación V4L2 y replay; datos fuera de git): frames + `frames.jsonl` por el camino real captura→inferencia→decisión→socket; gate ≥ 99 % idéntico a fuprobe, cos ≥ 0,99 por frame para **AuraFace y para SFace** (cada uno con su contrato de entrada de §2.6; el `score` SFace se compara con la salida de `fuprobe score` sobre los mismos crops), **el mismo `sequence` del frame que produce K=2** en `enroll-a/b` (no el `lit_index` bruto, §2.7) con el TTL de 800 ms evaluado sobre las `arrival` grabadas y sin descartar ningún acierto en esas sesiones, `spoof-phone` → `no_face`, metadato quitado → `unavailable metadata`; impostores: el scorer reproduce los máximos de E5 ±0,005 y, una vez en M2, `nirlockctl embed-dir` sobre LFW con el pipeline Rust reproduce los máximos con sus propios embeddings; **gate de enrolamiento**: re-enrolar `enroll-a/b` con `strict_gate` (0,80/0,03) debe conservar ≥ 90 % de las filas y mover el máximo impostor LFW ≤ 0,01, o la opción sigue apagada. CI de contribuidores con un retrato CC0 (determinismo, no precisión) y, si `libonnxruntime` falta, `vivid` en una lista blanca de driver **solo en pruebas** para ejercitar ioctls.
- **Lane PAM** (extiende `research/prototypes/pamtest`, sin root): lane real + módulo real contra un daemon falso (Python stdlib): `accept` 0, `reject` 7, `locked_out` 11, `unavailable` 7, `accept` de otro usuario 7, nonce malo 7, basura 7, línea larga 7, EOF 7, silencio 10 s 7, `error version` 7, socket escuchando sin daemon (≤ 2,5 s → 7), daemon que cierra tras `accept()` (sin `SIGPIPE`), SIGKILL a mitad (EOF en el daemon falso, sin estado); canarios fail-open (`ignore` + `optional pam_permit` = 0; `sufficient` + `pam_deny` aplana 11→7); **canario de confdir**: el arnés `t.c` extendido llama `pam_start_confdir(svc, user, conv, "/etc/pam.d")` con un servicio que existe **solo** en el directorio de proveedor (`systemd-run0` o `polkit-1`, presentes en `/usr/lib/pam.d` y ausentes en `/etc/pam.d` en esta máquina; la prueba lo comprueba antes) y demuestra que cae a `/etc/pam.d/other` → 7 (prueba negativa, sin root, que fija el hecho de §4.4; para hacerla independiente de la máquina se repite con un confdir temporal que contenga solo `other`), y la misma llamada con el confdir propio y `nirlock-lock` presente da el código del módulo; `PAM_IGNORE` y `pam_get_user` aparecen en `pam/*.c` **solo** en las dos líneas de guardia `#undef PAM_IGNORE` / `#pragma GCC poison PAM_IGNORE pam_get_user` (`grep -c PAM_IGNORE pam/*.c == 2`, comentarios incluidos en la prohibición; `make -C pam check-source`); arnés cuyo `conv` aborta si se invoca. **`nirlock-admin`** en el mismo arnés (sin root, con un `pam_unix` sobre un `passwd`/`shadow` de prueba vía `pam_start_confdir` y, donde no sea posible sin root, en M7 con sudo): contraseña correcta 0, incorrecta 7, usuario inexistente ≠ 0 (valor medido, anotado), archivo ausente → `other` → 7, conversación que recibe un estilo distinto de `ECHO_OFF` → `PAM_CONV_ERR`, nunca 0. E8b: las mismas lanes por `PamContext.configDirectory` en quickshell 0.3.1 **con el `configDirectory` distribuido** (copia de `/usr/lib/nirlock/pam.d` en `~/.local/share/nirlock-test/pam.d`), grabando la secuencia de señales para 0/7/9/11 (en particular si `PAM_MAXTRIES` llega como `MaxTries` o como `Error`; el wrapper no depende del resultado).
- **Sandbox real** (E10, M3, sudo): unidad transitoria con las directivas exactas: streaming IR+meta+RGB + inferencia bajo MDWE, lectura de journal, `boot_id`, D-Bus logind incluido `Inhibit("sleep", …, "delay")` con fd devuelto y un s2idle corto que espera al cierre de la cámara, `mlock`; `kill -SEGV` → el socket sigue existiendo y el módulo reconecta tras `RestartSec`; cliente de 30 líneas bajo el sandbox de `polkit-agent-helper@.service` intercambia `hello/welcome` (evidencia para v2).
- **Latencia** (`sudo nirlockctl bench --trials 8 --idle-seconds 0,10,60 --csv`, protocolo de fuprobe, batería y AC; `docs/BENCH.md`): tibio K=2 `cens.med ≤ 800 ms`, `p90 ≤ 1 100`, `cens.max ≤ 2 000`; frío `≤ 1 100` mediana; a oscuras con RGB `máx ≤ 1 000`; `models_ready` antes del primer frame si hubo `prewarm`; **E7** (3 ciclos s2idle cortos con `latency` al abrir la tapa) antes de congelar M6, y un presupuesto de resume medido en la lista de aceptación. Regresión > 50 ms sobre el último `bench.json` falla.
- **Lista manual de aceptación (firma de v1)**: (1) `nirlock-setup` en limpio, ≥ 60 filas, verificación acepta; (2) Super+Ctrl+L sentado: **sin** desbloqueo ≥ 10 s sin tocar nada; mover el ratón tras 3 s → ≤ 1,5 s; (3) tapa cerrada 30 s / 2 min / 10 min → desbloqueo ≤ presupuesto E7 tras encender la pantalla; (4) lock por `omarchy.idle` (300 s), volver: desbloqueo con la primera tecla, sin fuga de la tecla; (5) contraseña durante la ráfaga: el primero gana, sin "Checking…" colgado, sin doble desbloqueo; (6) cámara tapada: nada, `no_face`, sin fallos contados; (7) otra persona / foto en teléfono: `no_match`/`no_face`; 5 `no_match` → `Face locked: password`; la contraseña lo levanta (oráculo) o `reset-lockout`; (7b) **ciclar la tapa cada 0,5 s con otra cara** y **inclinar la foto** → los fallos se cuentan igual y llega el bloqueo; (7c) tercero al teclado con el dueño a 1 m detrás → `small_box`, sin desbloqueo (medir la distancia real); (8) `systemctl stop nirlockd.socket nirlockd.service`: lock intacto; (9) `FaceLane.qml` con error de sintaxis y `Service.qml` original renombrado: el lock sigue, `lock status` responde, `doctor` avisa, `--offline` restaura; (10) `omarchy-update`: el doctor diferido corre y el lock sigue; (11) navegador con `/dev/video0`: desbloqueo solo-IR; con `/dev/video2`: `camera_busy`; (12) `max_hours=0.01`: `Password to re-enable`; (13) reinicio: contadores según `boot_is_strong_auth`; primer lock tras autologin (#8762); (14) `nirlock-remove` deja `/etc/pam.d`, `shell.json` (tres claves) y unidades como estaban.

---

## 11. Cronograma

| Hito | Duración | Contenido | Aceptación |
|---|---|---|---|
| **M0** Toolchain y medición ORT — **✅ hecho 2026-09-23 (salvo la fila en batería)** | 1 día | rustup vía mise (Rust 1.98.1); workspace de 5 crates con lints de workspace (`undocumented_unsafe_blocks`, `unwrap_used`, `forbid(unsafe_code)` donde no hace falta); `ort` rc.13 `load-dynamic` + `api-27` contra ORT 1.29.1 (tarball oficial; Arch `onnxruntime-cpu` no estaba instalado — misma versión); YuNet, AuraFace y SFace sobre los PGM de `enroll-a` con paridad por frame contra OpenCV (`tools/bench-opencv`); `pam/` compila y su arnés mide los códigos crudos 9/10/3/7 de §4.3 con el lane empaquetado; `omarchy-plugin-validate` y `qmllint` pasan | ✅ `cargo test --workspace` verde (45 pruebas), `clippy -D warnings` limpio, `make -C pam test` verde; ✅ **AuraFace fp32 55–72 ms/frame a 4 hilos, primera inferencia +1–20 ms, carga 325–343 ms, RSS 490 MiB / pico 550 MiB bajo ORT, escritos en §1.5 y `docs/BENCH.md`** — **en AC; la fila en batería queda pendiente** (la máquina no se pudo desenchufar; el bench registra fuente, gobernador, EPP y `platform_profile` para que la fila sea comparable cuando se tome); ✅ `nirlock-bench` (carga + inferencia + paridad) bajo `MemoryDenyWriteExecute=yes` + `ProtectSystem=strict` + `PrivateDevices` + `NoNewPrivileges` + `RestrictAddressFamilies=AF_UNIX`: exit 0, latencias y paridad idénticas |
| **M1** Paridad de captura ✔ (2026-09-23) | 2 días | `nirlock-cam`: descubrimiento/pinning, GREY+UVCM, emparejado (gracia 25 ms + poda), buffer errors, palanca RGB, `meta_raw` en `record`; **LED de la cámara** (pregunta 5) observado un minuto con IR solo y con IR+RGB | 10 s en vivo: 0 metadatos perdidos, alternancia, primer frame ≤ 300 ms desde autosuspensión; fixtures reales del UVCM; v4l2loopback rechazado dos veces; **comportamiento del LED con IR solo y con IR+RGB, y visibilidad del emisor a oscuras, escritos en `docs/HARDWARE.md`** (decide si las ráfagas silenciosas por actividad son aceptables o exigen indicador antes de M6). **Resultado:** 10 s → 149 frames, 0 metadatos perdidos, 0 errores de buffer, 0 secuencias saltadas, alternancia estricta; 5 arranques en frío → primer frame mediana 253 ms, máx. 255, los cinco iluminados con `sequence=1`; paridad con `fuprobe` en las mismas condiciones; 3 fixtures reales del UVCM fijados en `uvcm.rs`; v4l2loopback rechazado en descubrimiento y en `QUERYCAP`; 114 tests, clippy limpio. **Hallazgo nuevo:** el RGB concurrente cuesta +198 ms de forma intermitente en frío (ADR-0006 corregido). **LED observado:** se enciende con solo IR, igual con RGB; el emisor se ve palpitar (`docs/HARDWARE.md`). M1 completo. |
| **M2** Paridad de visión | 3 días | YuNet decode/NMS, pose, gate, align, AuraFace/SFace por ORT (contratos de entrada de §2.6), FUEMB1; A/B int8; coste RGB en pieza iluminada (`rgb_assist` always vs auto); **E13** (NIR ambiental: ventana a pleno sol y halógeno: brillo de frames oscuros, detección en oscuros, valor de lit−dark, tasa de desacuerdo de los cruces FID/brillo en sombra); **E2-lite** (lente RGB tapada con cinta, IR en streaming con `rgb_assist=always`: ¿entra el firmware en el estado de privacidad/IR en blanco reportado en el hermano 0059?) | replays: gate ≥ 99 %, cos ≥ 0,99 (ambos embedders), mismo `sequence` en K=2, máximos E5 ±0,005 con embeddings Rust; AuraFace ≤ 200 ms/frame o int8 aprobado; E13 y E2-lite escritos en `docs/BENCH.md`/`HARDWARE.md` con decisión sobre `rgb_assist` |
| **M3** Daemon + IPC + CLI + sandbox | 4 días | servidor, peer creds, NDJSON, motor de petición (sin `Hold`), enrolamiento por IPC con fase `verify` interna, `bench`, inhibidor de retardo, **E10 con la unidad real** | `bench` K=2 mediana ≤ 800 ms tibio; prewarm oculta la carga; cancel por EOF cierra la cámara < 70 ms; fuzz 1 h limpio; E10 completo incl. `kill -SEGV` e `Inhibit` |
| **M4** Módulo PAM + regresión | 1 día | `pam_nirlock.so`, daemon falso, suite pamtest extendida (canario de confdir, `nirlock-admin`), E8b con el `configDirectory` distribuido | tabla de códigos exacta; sin `PAM_IGNORE`; SIGKILL sin estado; socket-sin-daemon ≤ 2,5 s; canario "vendor-dir-only cae a `other`" en rojo/verde |
| **M5** Política + persistencia + oráculo | 2 días | contadores, bloqueos, frescura, `boot_id`, cupos, seat, tapa/suspensión por logind, inhibidor, oráculo | máquina de estados probada (incl. ciclado de tapa, `accept` reinicia los tres contadores, ciclo de vida `lock` 60 s); en esta máquina (consentimiento): un desbloqueo por contraseña produce el registro `USER_AUTH … unix_chkpwd … res=success` con los campos `_AUDIT_FIELD_*` de §6.1 y el daemon lo consume; una línea forjada por `logger` no; anotado si el socket de auditoría bastó o hizo falta `Audit=yes`; `pkaction` confirma el permiso de `inhibit-delay-sleep` |
| **M6** Plugin wrapper | 2 días (segunda TTY abierta) | E9 con `configDirectory` de prueba, luego el real; **E7 medido antes** | `IdleMonitor` reporta actividad bajo `ext-session-lock` (si no: fallback a `resumed` + `enteredPassword`); lista 2–5, 7b, 8–9; Loader roto ejercitado; #8762; `grace`/`blankGuard` observados con timestamps |
| **M7** Empaquetado | 2 días (sudo) | PKGBUILDs (dos pkgbase), setup/remove, hooks, doctor (`configDirectory`, `--oracle`), `nirlock-admin` con `pam_unix` real | instalación limpia desde paquete local; `nirlock-setup` de punta a punta; `nirlock-remove` devuelve el sistema a stock (diff de `/etc/pam.d` = vacío, `shell.json`, unidades) |
| **M8** Aceptación y datos | 1–2 semanas de uso | ≥ 10 sesiones genuinas en días distintos (lentes, luz de día, tras resume) con `record`; latencia en batería y AC; sombra SFace revisada; cruces FID/brillo en sombra revisados; **E12** sobre las sesiones grabadas (¿se resuelve la retro-reflexión de pupila a 640x360 y 40–70 cm?; puntajes sombra de un PAD FLIR tipo DAMO si su licencia lo permite); umbral confirmado o corregido; SECURITY.md/THREATS.md | lista 1–14 completa; FRR por sesión ≤ 10 % a ≤ 2 s en pieza iluminada; 0 aceptaciones de pantalla; `doctor` limpio tras un `omarchy-update`; tasas de desacuerdo de los cruces y E12 en `docs/BENCH.md`; publicar (repo + AUR) y reportar en #8336/#5212 |

Total ≈ 17 días de trabajo + 1–2 semanas de uso. Dependencias externas: sudo del usuario en M3, M5, M7; consentimiento para el socket de auditoría en M5; segunda TTY en M6.

---

## 12. Riesgos

1. **Foto impresa (G2) sin probar**: la literatura da 97–100 % de éxito de impresiones láser contra reconocimiento NIR. La política Clase 2 (bloqueos, frescura, solo lock) es la única defensa y no hay PAD detrás. Si al medir pasa, se añaden cues deny-only (pupila brillante) o se baja `soft_lockout_after` a 3.
2. **Umbral 0,45 con una tarde de datos genuinos** y 14 identidades NIR; la FRR entre días puede forzar un umbral más bajo (come el margen de 0,26) o más alto (sube la FRR).
3. **Latencia bajo ORT sin medir**: todas las cifras de 150/634/570 ms son de OpenCV; ORT midió ~30 % más lento en la investigación. Si M0 da > 200 ms/frame, int8 (revalidar umbrales) o el crate `opencv` como plan B.
4. **Dos regímenes de auto-exposición a oscuras** sin causa conocida; el RGB concurrente es la única palanca medida (n=8) y su coste en pieza iluminada no está medido (M2; `rgb_assist=auto` por ALS como fallback).
5. **E7 (resume) sin medir**: el caso dominante en este portátil (tapa) depende de la reenumeración USB y del emisor tras s2idle; PR #5212 reporta el emisor "menos consistente tras suspender".
6. **`ort` es RC** acoplado a `onnxruntime-cpu` 1.29 por nivel de API; un bump del default `api-*` en la próxima rc rompería la carga hasta reconstruir (fail-closed, `doctor` lo dice).
7. **Contrato con el `Service.qml` original** (7 nombres) y con el mecanismo de plugins (semanas de vida, paquete semanal): un cambio deja sin rostro (benigno) o, si el wrapper mínimo no instancia, sin lock screen hasta que actúa la autocuración/doctor (grave; por eso el wrapper es de dos Loaders y hay hook diferido + post-boot).
8. **Quickshell 0.3.1**: #977 (invalid free en la ruta de conversación, no la usamos) y `abort()` con `waitpid` bloqueante en el hilo de UI.
9. **El oráculo de auditoría** exige activar la auditoría del kernel por journald; si un default futuro lo desactiva, la frescura vuelve a boot/root (más estricta, no menos segura, pero peor UX).
10. **uaccess sigue en `/dev/video2`**: cualquier app (incluidos clientes PipeWire) puede sostener el nodo IR y negar el rostro; quitarlo por udev (E11) tiene efectos sin medir en WirePlumber.
11. **Boot como SAE** solo mientras LUKS exija passphrase; el setup lo detecta una vez y `doctor` re-comprueba, pero un cambio manual posterior lo debilitaría en silencio si nadie corre `doctor`.
12. **`configDirectory` como dependencia**: el rostro funciona solo porque el `PamContext` del wrapper fija `/usr/lib/nirlock/pam.d`; si una versión futura de Quickshell quitara la propiedad o volviera a `pam_start()`, el lane caería a `other` → deny (fail-closed, sin rostro) y `doctor` lo diría. Ya no hay riesgo de que un `omarchy-apply-lock` futuro pise el lane: `/etc/pam.d` no se consulta para él.
13. **Ninguna señal de atención**: factor pasivo por diseño; publicado como tal.
14. **Bloqueo duro sin oráculo**: con `accept` reiniciando los contadores, el bloqueo duro exige 15 fallos seguidos sin ningún desbloqueo por rostro entre medias; sigue siendo posible (semana con lentes nuevos, cámara sucia) y entonces solo un reinicio o `sudo nirlockctl reset-lockout` lo levanta. El setup y el README lo dicen; la pregunta 2 (oráculo) es la forma de evitarlo.
15. **Inhibidor de retardo**: si polkit negara `inhibit-delay-sleep` al usuario `nirlock`, el cierre de la cámara antes del s2idle vuelve a ser "mejor esfuerzo" (riesgo de nodo colgado tras reset-resume, mitigado por el re-descubrimiento de §2.3).

---

## 13. Preguntas para el usuario

1. **Licencia**: ~~Apache-2.0 vs MIT~~ **Resuelto 2026-09-23: MIT para todo el código.** Los modelos conservan su licencia (YuNet MIT; SFace y AuraFace Apache-2.0) y se listan con atribución en `THIRD-PARTY.md`.
2. **Oráculo de auditoría**: **Resuelto 2026-09-23: sí**, el usuario autoriza `systemctl enable --now systemd-journald-audit.socket` cuando llegue M5/M7. Contexto original: ¿consentimiento para `systemctl enable --now systemd-journald-audit.socket` (auditoría del kernel hacia journald, sin auditd)? Es lo que permite que teclear la contraseña en el lock screen levante el bloqueo duro y refresque la frescura, como el PIN en Windows Hello. Sin él, solo reinicio (LUKS) o `sudo nirlockctl attest`.
3. **Frescura**: 24 h (con oráculo, invisible en el uso diario) ¿o 72 h si se declina el oráculo (este equipo se apaga a menudo)? Tope duro 7 días.
4. **Feedback visual**: **Resuelto 2026-09-23: solo texto corto vía `failureMessage` en v1.** Contexto original: ¿solo el texto corto vía `failureMessage` (borde rojo del campo de contraseña) en v1, o autorizar añadir la regla `above_lock` para el namespace `nirlock-lock-hint` en `~/.config/hypr/looknfeel.lua` y mostrar un icono? Cualquier capa que se vea sobre `ext-session-lock` es también un punto débil del compositor que conviene reportar a Hyprland.
5. **LED de la cámara**: **Resuelto 2026-09-23 (M1): sí.** El LED blanco se enciende con solo el nodo IR, sin diferencia perceptible al añadir el RGB, y el emisor se ve palpitar en rojo tenue. Una ráfaga de escaneo nunca es silenciosa, así que no hace falta indicador propio por privacidad (`docs/HARDWARE.md`). Pregunta original: ¿se enciende el LED blanco con solo el nodo IR en streaming, y con el RGB de asistencia? Es criterio de aceptación de M1 (un minuto de observación, resultado en `docs/HARDWARE.md`): decide si las ráfagas silenciosas por actividad son aceptables o necesitan indicador (pregunta 4) antes de M6. La pregunta aquí es si prefieres que se mire ya, con `fuprobe record`, sin esperar a M1.
6. **RGB de asistencia**: `always` (medido a oscuras, sin coste de latencia) ¿o `auto` solo con ALS < 20 lux? Depende del coste en pieza iluminada (M2).
7. **Presupuesto por lock**: 8 ráfagas por sesión de lock y 36 s de cámara por 5 minutos (8 × 4 s caben en un lock), ¿aceptable, o más conservador?
8. **Coredumps de fuprobe** en `/var/lib/systemd/coredump` (bench/ortbench, 19–20 sept): contienen frames IR y plantillas; ¿borrarlos con `coredumpctl`? (Fuera del alcance de este diseño; solo aviso.)
9. **E7 ahora**: ¿autorizas 3 ciclos s2idle cortos con `fuprobe latency` al abrir la tapa antes de M6? Es la única medición de Fase 0 que falta para el caso principal.
10. **uaccess** en `/dev/video2` (E11): ¿dejarlo en v1 (cualquier app puede producir `camera_busy`) y evaluar la retirada en v1.1?
11. **Umbral de saturación**: ¿autorizas medir en M2 si el gate puede pasar de 5 % a 10–15 % sin mover los puntajes de impostores (acortaría los desbloqueos a oscuras)?

---

## 14. Huecos de investigación: estado

Cada hueco que la síntesis dejó sin nombrar, con su estado. **Resuelto** = el diseño ya no depende de él o lo fija con una verificación hecha; **Diferido** = sigue abierto, con el hito en que se mide y qué pasa mientras tanto.

| Hueco | Estado | Dónde / qué |
|---|---|---|
| **E7** — primeros segundos tras resume s2idle / apertura de tapa (estrobo, `MetadataId 6`, tiempo al frame bien expuesto, renumeración USB) | **Diferido → antes de M6** (pregunta 9: 3 ciclos s2idle cortos con `fuprobe latency`). Hasta entonces el caso A (§1.5) no tiene número y el "sin re-armado por selector 9" de ADR-0005 es contingente. Mientras: el daemon espera la reaparición del nodo fijado ≤ 2 s dentro del presupuesto y el wrapper espera `availability_changed`; nada asume que el metadato sobrevive al resume | §1.5, §2.3, §11 M6 |
| **G2** — foto impresa (láser/inyección/brillante) y cues deny-only (pupila brillante, glint corneal) | **Diferido** (sin impresora). Única defensa: presupuesto de intentos (§6). sudo/polkit fuera de v1 por esto. Cues solo en sombra (E12, M8) | §8, §12.1, THREAT-MODEL T2 |
| **G3** — FAR ≤ 1e-4 en NIR (14 + 6 identidades NIR, una tarde de genuinos) | **Diferido → M8** (≥ 10 sesiones en días distintos: lentes, luz de día, tras resume). Umbral 0,45 provisional; Clase 2 hasta entonces | ADR-0007, §11 M8 |
| **ORT** — latencia, primera inferencia, carga, RSS en esta máquina | **Diferido → M0** (primer hito; todas las cifras de §1.5 son de OpenCV). Ningún número de aceptación de latencia vale antes | §1.5, §11 M0 |
| **E13** — NIR ambiental (sol, halógeno): brillo de frames oscuros, detección en oscuros, valor de lit−dark, seguridad de cualquier cruce FID/brillo | **Diferido → M2**. Mientras: los cruces FID/brillo son sombra, nunca regla (§2.4) | §2.4, §11 M2, THREAT-MODEL T19 |
| **E2** — estado de privacidad/IR en blanco del firmware con la lente RGB tapada o a oscuras (reportado en el hermano 0059) | **Diferido → M2 (E2-lite)**: cinta sobre la lente RGB, IR en streaming con `rgb_assist=always`. Importa más ahora que el RGB concurrente es la palanca de exposición | §11 M2, THREAT-MODEL T15b |
| Coste del RGB concurrente en pieza iluminada (solo medido a oscuras, n=8); `rgb_assist` always vs auto | **Diferido → M2** (A/B). Por defecto `always` hasta que el A/B diga otra cosa (pregunta 6) | §1.5, §11 M2, §13.6 |
| Régimen biestable de auto-exposición (cara quemada vs limpia en la misma escena) | **Diferido, sin experimento propio**: se observa en M8 con `record`; el RGB concurrente es la única palanca medida y v1 no escribe controles (ADR-0005). Si M8 muestra que el régimen quemado persiste con RGB, se abre una ADR para un ROI-write experimental | §12.4 |
| LED de la cámara con IR solo y con IR+RGB; visibilidad del emisor de noche (pregunta 5) | **Diferido → M1, criterio de aceptación** ("escrito en `docs/HARDWARE.md`"). Decide si las ráfagas silenciosas por actividad son aceptables o exigen indicador (pregunta 4) antes de M6 | §11 M1, §13.5 |
| **E11** — quitar `uaccess` de `/dev/video2` (efectos en WirePlumber) | **Diferido → v1.1** (pregunta 10). Mientras: cualquier app puede producir `camera_busy` (fail-closed) | §12.10, THREAT-MODEL T15 |
| **E12** — datos PAD en sombra (puntajes FLIR tipo DAMO, resolubilidad de la región ocular a 640x360 y 40–70 cm) | **Diferido → M8**, sobre las sesiones grabadas; sin gate en v1 | §11 M8 |
| **E10** — validación real del sandbox de la unidad (`MemoryDenyWriteExecute` con ORT, journal, D-Bus, `boot_id`, `Inhibit`) | **Diferido → M3** (primer criterio de aceptación). La unidad de §2.2 es una hipótesis probada solo directiva a directiva | §2.2, §10, §11 M3 |
| Mapeo de `PamResult` en Quickshell para `PAM_MAXTRIES` / `PAM_AUTHINFO_UNAVAIL` (E8b) | **Resuelto como no-dependencia**: el wrapper decide por `verify_finished`/`lockout_changed`; E8b mide el mapeo como dato (M4) | §4.3, §5.4 |
| `pam_start_confdir` vs directorio de proveedor | **Resuelto** [V] (shim `LD_PRELOAD`, 2026-09-23): confdir propio + `configDirectory`; canario en pamtest | §4.4, ADR-0012 |
| ¿Basta `systemd-journald-audit.socket` para encender la auditoría del kernel? Nombres exactos de campo de `USER_AUTH` de `unix_chkpwd` | **Diferido → M5**; el setup escribe `Audit=yes` si hace falta; filtro por `_AUDIT_FIELD_*`, no `_UID`; `doctor --oracle` lo prueba con un desbloqueo real | §6.1, §11 M5 |
| `IdleMonitor` (ext-idle-notify) bajo `ext-session-lock` en Hyprland 0.56 | **Diferido → M6** (primer criterio). Fallback esbozado: `resumed` + `enteredPassword` como disparadores; si tampoco, ráfaga única al `secure=true` + 3 s (peor UX, misma seguridad) | §5.3, §11 M6 |
| XU Realtek (unidad 4) alcanzable por la función RGB (`/dev/video0`, uid 1000): ¿influye en la función IR? | **Diferido → v1.1**; ahora en el modelo de amenazas como residual T7b. Ningún experimento con escrituras a esa XU en v1 (ADR-0005 y riesgo de brickeo) | THREAT-MODEL T7b |
| E14/E15/E16 (tecla de intención, benchmarks de prior-art, lecturas ACPI SDEV / descriptores MS OS 2.0) | **No necesarios para v1**; E14 (intención) vuelve con sudo/polkit (v2), E15 se hace al publicar `docs/PRIOR-ART.md`, E16 solo si un día se diseña el emparejado RGB↔IR | — |
| Longitud de onda / geometría del emisor y mapeo de campo visual RGB↔IR | **No necesarios para v1**; solo para una comprobación de co-localización RGB+IR (v2) | — |
| Quickshell `PamContext.configDirectory` y `pam_start_confdir` con `includes` | **Resuelto**: el lane no incluye nada; el confdir lleva su propio `other` | §4.4 |

---

## Apéndice A — Hallazgos de las revisiones y su resolución

| Hallazgo | Propuesta(s) | Resolución en este diseño |
|---|---|---|
| Contabilidad de intentos evadible (cancel/tapa/timeout/`inconclusive` gratis; decaimiento a cero) | SF-major, UX-blocker, MU-blocker+major | §2.7 clasificación al terminar por frames puntuados; §6.2 carga al primer frame bajo umbral, persistida antes del `result`; expiración del bloqueo blando no reinicia; ≤ 15 fallos entre SAE; cupo de frames/h; pruebas 7b |
| Wrapper se arma por presencia del lane, no por contenido | SF-major | §5.3 `FileView` sobre el archivo que el `PamContext` lee y comparación con el texto esperado; §4.4 lane en el confdir propio |
| **Lane en `/usr/lib/pam.d` nunca leído por Quickshell** (`pam_start_confdir` con `/etc/pam.d`, sin fallback al directorio de proveedor) | lista de correcciones 2026-09-23, bloqueante | §4.4 confdir propio `/usr/lib/nirlock/pam.d` + `configDirectory` en el `PamContext`; ADR-0012 reescrito; `doctor` comprueba el valor; canario de confdir en pamtest |
| `FaceLane` nunca envía `subscribe`; conexión `lock` cerrada a los 30 s | lista de correcciones, mayor | §5.3/§5.4 `subscribe` tras `hello`; PROTOCOL §3 exención y vida de `lock_session`; prueba de 60 s |
| Auto-verificación del enrolamiento imposible con la matriz de autoridad | lista de correcciones, mayor | §7 fase interna `verify` de `enroll`; `enroll_done.verified`; sin contadores ni cupos |
| `Hold` inalcanzable y con emisor encendido sin petición | lista de correcciones, mayor | §2.8 eliminado; §0 corregido; prueba de política |
| `nirlock-admin` sin texto de stack (riesgo de `faillock`/fail-open o `other`) | lista de correcciones, mayor | §4.4 verbatim, confdir propio, conversación restringida, casos en pamtest |
| Reglas de metadato sin medición (FID/brillo, +8, 10 %) | lista de correcciones, mayor | §2.4 solo reglas de Fase 0; cruces en sombra; E13 en M2 |
| Orden de apertura, gracia de 25 ms, poda y `lit_index` sin especificar | lista de correcciones, mayor | §2.3 secuencia exacta; §2.4 emparejado; §2.7 entradas de ventana y TTL sobre `arrival` |
| `timeout=7000` < peor caso 7 200 | lista de correcciones, menor | §4.2 `budget_ms` dinámico; PROTOCOL §8 |
| 8 × 4 s > 30 s de cámara; `rate_limited` sin fila | lista de correcciones, menor | §6.3 36 s; §5.4 fila `rate_limited` + `retry_after_ms` |
| `armed` con `Date.now()`; `blankGuard` sin arranque | lista de correcciones, menor | §5.3 `Timer grace` + `graceElapsed`; `blankGuard.restart()` en ráfaga y actividad |
| `MaxTries` como dependencia del wrapper | lista de correcciones, menor | §4.3 [H]; §5.4 bloqueo por eventos del daemon |
| Filtro `_UID` del oráculo no coincide nunca | lista de correcciones, menor | §6.1 `_AUDIT_FIELD_UID`; `Audit=yes` si hace falta; `doctor --oracle` |
| `PrepareForSleep(true)` sin inhibidor de retardo | lista de correcciones, menor | §2.3 punto 5 `Inhibit("sleep", …, "delay")` |
| Contrato de entrada de SFace no declarado | lista de correcciones, menor | §2.6 contratos; paridad SFace en §10 |
| `DESIGN-v1.md` sin aviso de superación | lista de correcciones, menor | primera línea de `DESIGN-v1.md` |
| T7b (XU Realtek por la función RGB), T15b (lente RGB tapada), `account_locked` | lista de correcciones, menor | THREAT-MODEL T7b/T15b; PROTOCOL §6.1; E2-lite en M2 |
| E12/E13 sin programar; LED solo como pregunta | lista de correcciones, menor | §11 M1/M2/M8 |
| `failures_since_sae` nunca bajaba con `accept` | lista de correcciones, menor | §6.2/§6.3 y ADR-0009: `accept` reinicia los tres contadores |
| "Caja 95–140 px" y gate de enrolamiento 0,80/0,03 sin validar | lista de correcciones, menor | §2.5 rangos por sesión; gate de Fase 0 por defecto; replay en §10 |
| `ProcSubset=pid` oculta `boot_id`/lid/cpuinfo | SF, MU, ambos feasibility | §2.2 `ProcSubset=all`; tapa y suspensión por logind; `boot_id` ilegible = bloqueo duro |
| Pérdida del servicio de lock (FaceLane inline, `OMARCHY_PATH` vacío, updates fuera de `omarchy-update`, borrar el dir antes de deshabilitar) | SF-major, UX-major, MU-major, omarchy-realism blocker | §5.2 dos Loaders + autocuración; `stockUrl` desde `env` en la declaración; `Binding` para `shell`/`omarchyPath`; §5.7 deshabilitar antes de borrar, tres claves de `shell.json`, rechazo con sesión bloqueada; §5.8 doctor diferido + post-boot; nunca `omarchy-refresh-shell` |
| Retiro basado en #6863 / `"faceConfigured" in item` | omarchy-realism major | §5.7 ceder solo con `faceConfigured === true`; #8336 es Howdy |
| Doctor post-update interroga al shell viejo | omarchy-realism major ×2 | §5.8 chequeo estático en el hook + `systemd-run --on-active=60s` + post-boot con sondeo 30 s |
| Disparo solo por flanco; no se distingue `unavailable` de `reject` | omarchy-realism major | §5.3 timer de presencia (nivel); evento `verify_finished`; espera de `availability_changed` tras resume |
| Puntajes en journal legible por `wheel`; `hits`/`gate` en `progress`; `status` con edad del SAE | SF-minor, UX-major, MU-major | §2.10 puntajes solo en `audit.jsonl` 0600; `progress` solo `phase`; `status` no-root sin SAE |
| Replay del oráculo tras pérdida del cursor | SF-minor | §6.1 `_BOOT_ID` actual + marca de agua "ahora" |
| `pam_get_user` abre la conversación | SF, UX, MU, feasibility | §4.2 `pam_get_item(PAM_USER)` + `#pragma GCC poison` |
| `SIGPIPE` en el módulo | SF-minor | §4.2 `send(MSG_NOSIGNAL)` |
| `SO_PEERCRED` = euid: sudo/polkit conectarían como root en v2 | SF-minor, MU-minor | §3/PROTOCOL: autoridad por (uid, `client`); `pam` nunca administra; `verify` de uid 0 exige `ruser` |
| `welcome` en 500 ms falla en arranque frío; socket escuchando sin daemon espera el presupuesto | SF-minor, MU-major | §4.2 `welcome` inmediato desde `Starting`, espera ≤ 2 500 ms; sin `ConditionPathExists` |
| Gracia tras el lock; armado antes de `secure` | SF-minor, omarchy-realism minor ×2 | §5.3 latch de `secure=true` + 3 s |
| `setrlimit` bajo `~@resources`; loop de reinicio por config; hash en cada prewarm; sin límite de mensajes | SF-minor, feasibility | §2.1/§2.2/§3 |
| `backup=` de un archivo no instalado; `pacman -R` sin limpieza | SF-minor, UX-minor | §9.4 |
| `PrepareForSleep(true)` sin manejar; sin productor de evento de tapa | SF-minor, omarchy-realism minor | §2.3 puntos 5–6 |
| PREWARM sin STREAMON no despierta el USB en 7.2.5 | feasibility (UX) major | §1.5 caso A sin promesa; prewarm solo de modelos; keepalive/RGB-STREAMON como experimento v1.1 (pregunta 5) |
| Cifras de latencia son de OpenCV, no ORT; primera inferencia lenta | feasibility ×3 | §1.5 tabla honesta; M0 mide; palancas listadas |
| `ort` sin fijar `api-*`/features; `download-binaries` | feasibility ×3 | §2.6 |
| `MemoryHigh=600M` con RSS sin medir | feasibility (UX) | §2.2 900M/1200M, M0 mide |
| Split package comparte `pkgver`; `raw.githubusercontent` sirve punteros LFS; commits sin fijar | feasibility ×2 | §9.3 |
| SFace sombra antes de la decisión; `Nice=-2` | feasibility | §2.7, §2.2 |
| Fixtures UVCM inexistentes; impostores solo con el scorer | feasibility | §2.4, §10 |
| Offset del escáner C / serde | feasibility | §3 formato fijo |
| `warm_ttl` descarga antes del regreso; `WaitModels→Opening` en serie | feasibility (MU) blocker | §2.1 residente mientras hay lock; §2.6 cámara en paralelo con la carga |
| `RuntimeDirectory=` borra el socket | feasibility (MU) blocker, security (UX/MU) | §2.2 sin `RuntimeDirectory` |
| Orden RGB/IR distinto del medido | feasibility (MU) | §2.3 punto 4 |
| Coredumps con datos biométricos; `panic=abort` sin `LimitCORE` | security (UX) | §2.1/§2.2; pregunta 8 |
| Claims falsos "cámara solo con lock" / "uid 1000 no puede desbloquear" | security (UX) | §1.2, §8, THREAT-MODEL |
| `cancel` por `ref` adivinable | security (UX) | PROTOCOL: `cancel` exige nonce |
| Caché de sudo como ancla fuerte | security (UX) | §6.1 (b) `nirlock-admin` |
| `StartLimit`, slowloris, relojes unix, `--debug-dump`, `RuntimeDirectoryPreserve` | security (UX) | §2.2, §3, §6.1, §2.10 |
| Boot-SAE con TPM/keyfile | security (MU) | §6.1 (a) `luksDump` en el setup |
| `stale` → `MAXTRIES` sin `retry_after` | security (MU) | §4.3 `unavailable` |
| Cuenta bloqueada sin `acct_mgmt`; overlay sobre ext-session-lock | security (MU) | §6.3, THREAT-MODEL, pregunta 4 |
| Doble señal `error`+`completed`; `Socket.connected` como binding; `send()` inexistente | omarchy-realism ×2, feasibility | §5.3 |
| `failureMessage` largo/estilo de error; ráfagas contra pantalla apagada | omarchy-realism, feasibility | §5.5 (≤ 22 chars, `runWake()`), §5.4 `blankGuard` |
| Scripts `omarchy-*` en el paquete; enable antes del rescan asíncrono | omarchy-realism | §9.4, §9.5 |
| hypridle no existe aquí | omarchy-realism | `omarchy.idle` en todo el texto |
| `embed_workers=2`; fp16 | feasibility (UX) | rechazado; int8 en M2 |
| Two conflicting documents (DESIGN-v1.md vs proposal) | security (MU) | este DESIGN.md sustituye a DESIGN-v1.md |

**Hallazgos rechazados (con motivo):**
- *Usar `enteredPassword` como disparador* (MU): descartado; `IdleMonitor` cubre teclado y ratón sin tocar el búfer de la contraseña.
- *Prefijo de longitud en el marco* (UX, MU): descartado; desde QML no se conoce la longitud en bytes; NDJSON con límite de línea da la misma cota.
- *`DynamicUser=yes`* (SF, UX): descartado por usuario estático `nirlock` (uid predecible, `sudo -u nirlock` para diagnóstico, sin interacción con `RuntimeDirectory`, sysusers estándar en Arch). La dureza es la misma.
- *Overlay `PanelWindow` como indicador v1* (MU): descartado; invisible sin `above_lock` y potencialmente una debilidad del compositor. Pregunta 4.
- *Escanear al bloquear o en `runWake()` como #8336* (UX rechazado, MU): descartado; privacidad y "auto-desbloqueo" con el usuario sentado.
- *Lane instalado por el setup en `/etc/pam.d`* (SF, UX; y opción (b) de la lista de correcciones): descartado por el directorio propio `/usr/lib/nirlock/pam.d` del paquete (verificable con `pacman -Qkk`, deja `/etc` libre para upstream, sin `backup=` ni conflictos por contenido con #8336). `/usr/lib/pam.d` también se descartó: Quickshell no lo consulta (§4.4). La consecuencia "el lane existe antes de enrolar" es inocua: sin plantilla el daemon responde `not_enrolled` y el wrapper no se habilita hasta el setup.
- *`Hold` "real" (un `verify` inmediato exento de `verify_min_interval_ms`)* (lista de correcciones, alternativa): descartado; añadiría una excepción al cupo y segundos de emisor sin petición para ganar ~70 ms (253 → 180) en un reintento que ya es barato porque el USB no autosuspende hasta 2,6 s tras el cierre.
- *Contar frames puntuados (no ráfagas) para el bloqueo* (MU): adoptado a medias. Los bloqueos siguen contando ráfagas fallidas (predecible para el usuario), pero la carga se hace al primer frame puntuado y hay un cupo de frames puntuados por hora, lo que cierra el mismo agujero.
- *`tract` como backend opcional obligatorio* (MU): no en v1; anotado como plan C.
