
## 2026-09-27 — política de bloqueos (elegida por el usuario)

Dos capas, con el modelo mental del usuario: "algunos intentos y luego solo
contraseña; la gracia de Hello es abrir la tapa y entrar".

1. **Por sesión de bloqueo: 5 intentos** (en el plugin). Agotados, esa
   sesión es solo contraseña y la cámara no se enciende más; el mensaje lo
   dice en la pantalla. Se reinicia al desbloquear. Es seguro porque no se
   pueden conseguir intentos nuevos sin desbloquear primero.
2. **Persistente: 10 fallos consecutivos** (en el daemon, `state.json`).
   Sobrevive a reinicios del daemon y del equipo. Se levanta con un acierto,
   con un arranque nuevo (que exige la frase de LUKS) o con `reset-lockout`.
   Un archivo de estado corrupto o borrado **bloquea**, no abre.

Solo cuenta como fallo una petición que puntuó al menos un frame bajo el
umbral. "Nadie a la vista" no cuenta: si contara, cualquiera podría dejar al
dueño fuera tapando el lente.

**Frescura por tiempo: desactivada**, por decisión del usuario. Lo que se
cede es la cota sobre cuánto tiempo sigue sirviendo una foto impresa en un
equipo robado encendido y bloqueado — y esa es justo la prueba que no hemos
podido hacer. Lo que lo hace defendible: el disco está cifrado con LUKS, así
que un equipo reiniciado pide la frase y el rostro nunca entra en juego; y
el bloqueo por fallos consecutivos sigue puesto, así que no son intentos
infinitos.
