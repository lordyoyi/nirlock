# nirlock — instalación y rescate

Desbloqueo facial por infrarrojo para la pantalla de bloqueo de Omarchy,
en el Zenbook UX3405CA.

Código: `~/Work/nirlock` (GitHub: lordyoyi/nirlock)
Fecha de estas instrucciones: 27 de septiembre de 2026

**Este archivo está en Dropbox a propósito**: si la pantalla de bloqueo
falla, lo puedes leer desde otro dispositivo o desde una consola de texto.
Desde una consola, para leerlo:

    less "$HOME/Dropbox/01 - Proyectos/nirlock - instalacion y rescate.md"

(Se sale con `q`.)

---

## PRIMERO: cómo moverse entre consola y escritorio

- **Ctrl+Alt+F3** te lleva a una consola de texto. Ahí escribes tu usuario,
  Enter, tu contraseña, Enter. La contraseña no se ve mientras la escribes:
  es normal, sigue escribiendo.
- **Ctrl+Alt+F1** (o **F2**) te devuelve al escritorio.
- La sesión de la consola queda abierta aunque vuelvas al escritorio. Esa es
  toda la gracia: es la red de rescate esperando por si acaso.
- Para cerrar esa sesión cuando ya no la necesites: en la consola escribe
  `exit` y vuelve al escritorio.

**No tienes que ejecutar nada en la consola durante la instalación.** Los
comandos de instalación van en tu terminal normal, en el escritorio. La
consola es solo el seguro.

---

## INSTALACIÓN

Todos estos comandos van en la terminal del escritorio, uno por paso.

### 1. Deja la consola de rescate abierta

Ctrl+Alt+F3, inicia sesión, y vuelve al escritorio con Ctrl+Alt+F1.

### 2. Compila

    cd ~/Work/nirlock && cargo build --release --locked && make -C pam

### 3. Descarga los modelos (solo la primera vez, 286 MB)

    cd ~/Work/nirlock && scripts/nirlock-fetch-models

No pide sudo. Verifica cada archivo contra un SHA-256 fijado; si alguno no
cuadra, aborta y no deja nada.

### 4. Instala (único paso con sudo)

    sudo ~/Work/nirlock/scripts/nirlock-install

Hace todo: el usuario de sistema, el servicio, el módulo PAM, el plugin de
la pantalla de bloqueo y la entrada del menú. Si ya estabas enrolado, deja
tu enrolamiento intacto.

Debe terminar diciendo `Face unlock is set up.` Si en la línea del plugin
dice `installed, but NOT enabled`, trae el motivo pegado: léelo.

### 5. Comprueba que el servicio quedó corriendo

    systemctl status nirlockd --no-pager | head -5

Debe decir `active (running)`, y la hora de arranque debe ser de hace
segundos. **Ojo con esto**: si dice una hora vieja, el binario nuevo no está
en uso.

### 6. Comprueba que la cámara es reconocida

    nirlockctl probe

Debe terminar en `-> supported: profile 'shinetech-3277-0055'`.

### 7. Comprueba que la contraseña sigue funcionando (ANTES del rostro)

Bloquea con **Super+Ctrl+L**. Debe aparecer el campo de contraseña.
Desbloquea escribiéndola, como siempre.

Si el campo aparece, lo riesgoso ya pasó.
Si la pantalla queda negra o sin campo, ve a la sección RESCATE.

### 8. Prueba el rostro

Bloquea con **Super+Ctrl+L** y mira la pantalla.

El escaneo empieza 1 segundo después de bloquear, a propósito: le da tiempo
a pasar a la tecla con la que bloqueaste. Verás la franja synthwave abajo y
la palabra `SCANNING`.

Debería desbloquearse en menos de un segundo. La contraseña sigue
funcionando siempre, en paralelo.

---

## RESCATE

### Si la pantalla de bloqueo falla pero el escritorio funciona

En la terminal:

    omarchy-plugin-disable nirlock.lock

Omarchy restaura el bloqueo original solo, porque anotó que lo había
desactivado por culpa del plugin nuevo.

### Si no puedes llegar al escritorio

Ctrl+Alt+F3, inicia sesión, y ahí:

    omarchy-plugin-disable nirlock.lock

### Si eso falla (el shell de Omarchy se cayó y no responde)

En la consola:

    cp ~/Work/nirlock-respaldos/shell.json.antes-de-nirlock ~/.config/omarchy/shell.json

Y reinicia:

    reboot

Ese archivo es una copia de tu configuración del 27 de septiembre, antes de
instalar nada. No depende de que el shell funcione.

### Si el rostro se bloqueó por demasiados fallos seguidos

La contraseña siempre entra. Ya dentro, para reactivar el rostro:

    nirlockctl reset-lockout

### Si quieres desinstalarlo todo

    sudo ~/Work/nirlock/scripts/nirlock-uninstall

Comprueba que tu pantalla de bloqueo original volvió **antes** de borrar
nada, y aborta si no puede confirmarlo.

---

## DIAGNÓSTICO

### El rostro no desbloquea

Ver qué dijo el daemon (servicio de sistema, sin `--user`):

    journalctl -u nirlockd -n 30 --no-pager

Ver qué dijo el módulo PAM:

    journalctl -n 30 --no-pager | grep pam_nirlock

Razones de rechazo que puede reportar:

- `no_face` — no te vio. Puede ser que no estabas mirando, o poca luz de
  frente.
- `pose` — te vio pero de perfil, o con la cabeza inclinada.
- `saturated` — la cámara sobreexpuso tu cara. Pasa en piezas oscuras.
- `below_threshold` — te vio bien pero no coincidió con la plantilla.
- `camera_busy` — otra aplicación está usando la cámara.

### El servicio no arranca

    systemctl status nirlockd --no-pager -l

### Volver a enrolar

    nirlockctl enroll

### Si tocaste el QML del plugin y no cambia nada

    omarchy restart shell

Desactivar y reactivar el plugin **no** recarga el código: Quickshell guarda
los tipos QML ya compilados y vuelve a instanciar el mismo código viejo.

---

## ESTADO (al 27 de septiembre de 2026)

Lo que ya está:

- Corre como servicio de sistema con su propio usuario sin privilegios. Tu
  plantilla facial vive en `/var/lib/nirlock` y **no** es legible por tu
  usuario de escritorio.
- Hay política de bloqueo: tras demasiados fallos seguidos que sí vieron una
  cara, el rostro se desactiva y solo entra la contraseña.
- Desbloquea entre 0,5 y 1,7 segundos.

Lo que falta:

- El umbral de reconocimiento (0,45) está calibrado con datos de una sola
  tarde. Las próximas semanas de uso son las que lo confirman o lo corrigen.
- No está probado contra fotos impresas. Por eso el rostro solo desbloquea
  la pantalla, nunca `sudo` ni la contraseña del disco.

Lo que sí está medido: una pantalla de teléfono con tu foto no lo engaña (en
infrarrojo no se ve), y de 4.875 personas distintas ninguna se acercó a tu
puntaje.
