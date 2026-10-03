# nirlock — contexto para Claude

Desbloqueo facial por infrarrojo (estilo Windows Hello) para la pantalla de
bloqueo de Omarchy. Daemon Rust con sandbox + módulo PAM en C + plugin QML.
Leer primero: `README.md`, `DECISIONS.md`, `docs/DESIGN.md` y `docs/adr/`.
Este archivo resume lo que no se deduce del código.

## Equipos donde se trabaja

Rodrigo trabaja en esto desde más de un equipo y retoma sesiones de Claude en
cualquiera de ellos. **Este archivo es la memoria compartida entre equipos**:
al terminar una sesión con decisiones o estado nuevo, actualiza la sección
"Estado y pendientes" de abajo y haz commit.

- **Zenbook UX3405CA** — la máquina de referencia y la única con cámara IR
  (Shinetech `3277:0055`). Checkout en `~/Work/nirlock`. Es donde nirlock está
  instalado y se usa a diario; todo lo medido en `docs/` viene de aquí.
- **notro** (Beelink, Arch + Omarchy, siempre encendido) — checkout en
  `~/Dev/nirlock`. **No tiene ninguna cámara** (`/dev/video*` no existe) ni
  `onnxruntime-cpu`: sirve para compilar, tests, clippy y revisar
  instalación/scripts, nunca para probar captura, enrolamiento o desbloqueo.

## Comandos

```
cargo build --release --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
make -C pam && make -C pam test
```

- `cargo fmt --check` **no está limpio en master** (código anterior sin
  formatear). No reformatees todo dentro de un PR de otra cosa; si se hace,
  que sea un commit propio.
- `nirlockctl probe` desde el checkout (`target/release/nirlockctl probe`)
  ve el perfil embebido `hw/3277-0055.toml` más `/etc/nirlock/hw/`.
- Cambios en QML del plugin requieren `omarchy restart shell`; desactivar y
  reactivar el plugin NO recarga el código (caché de Quickshell).

## Reglas que no se negocian

- **La contraseña siempre funciona en paralelo.** Ningún cambio puede dejar
  la pantalla de bloqueo sin campo de contraseña. Antes de tocar el plugin o
  PAM en el Zenbook, deja una consola de rescate abierta (Ctrl+Alt+F3);
  procedimiento en `docs/INSTALACION-Y-RESCATE.md`.
- El rostro es solo para la pantalla de bloqueo: nunca LUKS, sudo, polkit ni
  login (`docs/POR-QUE-NO-EL-LOGIN.md`).
- `nirlock-cam` no escribe controles UVC ni sondea extension units, jamás
  (ADR-0005: cámaras hermanas se han dañado así).
- El módulo PAM nunca devuelve `PAM_IGNORE`; todo fallo termina en
  `pam_deny`. Los archivos de `pam.d/` se instalan **byte a byte desde el
  repo** (el plugin solo se arma si `nirlock-lock` es idéntico).
- Frames, plantillas y dumps de bench son datos biométricos: van en un repo
  privado aparte, nunca aquí (ver `.gitignore`).

## Convenciones

- Código, comentarios, README y ADRs en inglés; `DESIGN.md`, `DECISIONS.md`,
  la guía de instalación y los mensajes de commit en español.
- Mensajes de commit: título corto en español en lenguaje natural ("Una vía
  para que la gente mande su cámara"), cuerpo que explica el porqué.
- Comentarios densos que cuentan el incidente que motivó cada decisión
  ("medido el 2026-09-27: ..."). Mantener ese estilo.
- Decisiones de política del usuario → `DECISIONS.md`; decisiones de diseño →
  nuevo ADR en `docs/adr/`.

## Estado y pendientes

*Actualizado: 2026-10-03, desde notro.*

- Funciona a diario en el Zenbook (0,5–1,7 s). Verificado en **una** sola
  cámara. Pendiente: prueba con foto impresa y validar el umbral 0,45.
- **Primer usuario externo (vía Discord, 2026-10-03):** ThinkPad con
  `5986:212b`, webcam solo RGB, sin sensor IR → no puede funcionar. Destapó:
  README pedía el paquete inexistente `onnxruntime` (es `onnxruntime-cpu`),
  el instalador no comprobaba nada antes de instalar, el daemon quedaba en
  bucle de reinicios sin cámara, y el instalador no instalaba
  `pam.d/nirlock-admin` y escribía un `other` recortado a mano. Arreglado en
  la rama `fix/install-preflight-and-pam` (issue #1).
- **Por validar en el Zenbook** después de mergear ese PR: reinstalar
  (`sudo scripts/nirlock-install`) y confirmar que la revisión previa dice
  `supported`, que `/usr/lib/nirlock/pam.d/` tiene los 3 archivos y que el
  bloqueo con contraseña y con rostro siguen funcionando.
- Deuda conocida: `reset-lockout` aún no hace la verificación PAM
  `nirlock-admin` que pide DESIGN §6.1 (hoy basta con root); `attest` y
  `delete` no están implementados. Más adelante, el daemon sin cámara
  debería quedarse vivo y responder `unavailable` en vez de salir con 78.
