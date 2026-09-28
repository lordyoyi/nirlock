# Por qué nirlock no abre el inicio de sesión ni el disco

La pregunta llega sola: "si desbloquea la pantalla, ¿por qué no me deja
entrar al computador con la cara?". La respuesta corta es que en la mayoría
de los equipos **no hay ningún login al que agregárselo**, y que donde sí lo
hay, el rostro no puede hacer el trabajo que se le pide.

## El arranque típico de Omarchy

    encender → frase de LUKS → autologin → escritorio

`/etc/pam.d/sddm-autologin` usa `pam_permit.so`, que deja pasar sin
preguntar. Lo único que se escribe al encender es la frase del disco.

## Por qué el rostro no puede reemplazar la frase de LUKS

Dos razones, y la segunda es la de fondo:

1. **Dónde ocurre.** La frase se pide en el initramfs, antes de que exista
   el sistema: no hay stack de cámara, ni runtime de inferencia, ni daemon,
   ni las plantillas (viven en el disco que todavía está cifrado).
2. **Qué es un factor biométrico.** El rostro no *libera* una llave, solo
   responde sí o no. LUKS necesita una llave real. Un "sí" no descifra nada.

**Windows Hello tampoco hace esto.** BitLocker lo abre el TPM; Hello
desbloquea la *sesión*. Lo que se siente como "entro con la cara" son dos
mecanismos distintos: el TPM entrega la llave del disco sin preguntar, y la
cara protege la sesión que viene después.

## El equivalente real, y su condición

Sellar la llave de LUKS en el TPM (`systemd-cryptenroll --tpm2-device=auto`)
para que el arranque no pida frase, y dejar que el rostro proteja la sesión.

**Solo tiene sentido con Secure Boot activo.** Sin él, el TPM entrega la
llave a cualquier kernel que arranque, incluido uno modificado por quien
tenga el equipo en la mano: protege contra sacar el disco y nada más. En el
equipo donde se desarrolló esto, Secure Boot está deshabilitado, así que
sellar la llave habría sido teatro. Es una decisión de cifrado de disco, no
una función de nirlock.

## Lo que sí se podría agregar: el greeter de SDDM

Para quien **no** usa autologin, un carril de rostro en `/etc/pam.d/sddm`
haría que seleccionar el usuario y pulsar Enter entre sin escribir la
contraseña.

No está implementado, y hay un obstáculo de seguridad real que resolver
antes: el greeter corre como el usuario `sddm`, no como la persona. La regla
del daemon es que el par del socket solo puede pedir verificación **de sí
mismo** (`SO_PEERCRED`, frontera B1 del diseño), así que hoy responde
`wrong_user`. Habilitarlo exige permitir que un greeter declarado de
confianza pida por otro usuario, lo que es una excepción a la regla que
sostiene todo lo demás y merece su propio ADR.

Dos advertencias más para ese camino:

- `/etc/pam.d/sddm` incluye `system-login` → `system-auth`, cuya línea
  `auth optional pam_permit.so` convierte un resultado "ignorado" en éxito.
  Un carril mal escrito ahí **abre la sesión a cualquiera**. El módulo de
  nirlock nunca devuelve ese código, pero el margen de error es cero.
- El greeter no sabe quién eres hasta que eliges usuario, así que el rostro
  solo puede verificar, no identificar.
