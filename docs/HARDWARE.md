# Hardware — la máquina de referencia, y cómo añadir la tuya

La primera parte documenta la máquina donde se desarrolló esto: ASUS Zenbook
UX3405CA, cámara Shinetech `3277:0055`. La última sección, **Otras cámaras**,
es la que importa si la tuya es distinta.

Todo lo de aquí está **medido en esta máquina**, salvo lo marcado como
PENDIENTE. Kernel 7.2.5-3-omarchy, `uvcvideo`, `nodrop=1`.

## Identidad y nodos

| Dato | Valor |
|---|---|
| USB | `3277:0055` (Shinetech, módulo Realtek), `bcdDevice 0103` |
| Ruta sysfs fijada | `/sys/devices/pci0000:00/0000:00:14.0/usb3/3-9`, `removable=fixed` |
| Interfaz 00 | `video0` (RGB, `index 0`), `video1` (metadatos, `index 1`) |
| Interfaz 02 | `video2` (IR, `index 0`), `video3` (metadatos, `index 1`) |
| Formato IR | `GREY` 640x360 @15 fps, único; 230.400 bytes por frame |
| Formato metadatos IR | `UVCH` por defecto, `UVCM` tras `S_FMT` (= `V4L2_META_FMT_UVC_MSXU_1_5`, kernel ≥ 6.17) |
| Formato RGB | `MJPG` hasta 1920x1080 @30; usamos 1280x720 |

**Nunca tocar:** la extension unit propietaria de Realtek (unidades 4/10/11,
presentes en ambas funciones) y la interfaz USB DFU (interfaz 4). Cámaras
hermanas se han dañado sondeando XUs a ciegas. `nirlock-cam` no emite ninguna
consulta ni escritura de control: ADR-0005 y el test
`unsafe_and_ioctl_surface_is_the_declared_one`.

## Emisor y etiquetado

El emisor IR estroboscopea **por defecto del firmware**, sin ninguna escritura
de control, alternando estrictamente iluminado/oscuro. Cada frame trae el
metadato de Microsoft `MetadataId = 6` (`FrameIllumination`), tamaño 16, bit 0
= iluminado. Coincidió con el brillo en 480/480 frames de la Fase 0 y en
148/148 de la validación de M1.

El primer buffer tras `STREAMON` llega con **dos bloques y el FID mezclado**
(la cabecera de un slot saltado precede a la carga útil iluminada); el parser
descarta los bloques anteriores al cambio de FID. Buffers reales fijados como
fixtures en `crates/nirlock-cam/src/uvcm.rs`
(`real_camera_buffers_parse_as_recorded`), capturados el 2026-09-23:

| Secuencia | Bloques | Ítems | Bytes | FID mezclado | Etiqueta |
|---|---|---|---|---|---|
| 1 | 2 | 1 | 60 | sí | iluminado |
| 2 | 1 | 1 | 38 | no | oscuro |
| 3 | 1 | 1 | 38 | no | iluminado |

## Tiempos (M1, `nirlockctl record`, batería)

Cinco arranques en frío de 2 s, con ≥ 7 s entre ellos para que el USB entre en
autosuspensión (2,6 s tras cerrar):

| | Mediana | Máximo |
|---|---|---|
| `open` → `STREAMON` completo | 112,7 ms | 115 ms |
| Primer frame entregado | **253 ms** | 255 ms |

Los cinco: primer frame **iluminado**, `sequence = 1`, 0 metadatos perdidos, 0
errores de buffer, 0 secuencias saltadas. Un frame iluminado cada 133 ms.
Idéntico a `fuprobe` en las mismas condiciones (253/254 ms), que es el
criterio de paridad de M1.

## RGB concurrente: penalización intermitente de ~200 ms

ADR-0006 afirmaba coste cero en latencia, medido en caliente en la Fase 0. En
frío **no siempre es cierto**. Tres arranques en frío con `--rgb`:

| Corrida | Primer frame IR | `first_sequence` | RGB |
|---|---|---|---|
| 1 | 253 ms | 1 | 112 frames, 29,6 fps, 0 errores |
| 2 | **451 ms** | 1 | 114 frames, 29,6 fps, 0 errores |
| 3 | 253 ms | 1 | 112 frames, 29,6 fps, 0 errores |

`first_sequence = 1` también en la corrida lenta: **no se perdieron frames**,
el sensor IR simplemente empezó a entregar ~198 ms más tarde (≈ 3 slots de
66,7 ms). Ocurrió 1 de 3 veces aquí y 1 de 1 en la corrida del implementador,
así que es intermitente y no depende de la carga de CPU (el hilo RGB no
decodifica nada).

Consecuencia para el presupuesto: con `rgb_assist` el peor caso del primer
frame sube de ~255 ms a ~455 ms, es decir un desbloqueo K=2 de ~800 ms en vez
de ~600 ms. Sigue dentro del presupuesto de 4 s, pero ADR-0006 debe decir
"coste cero en la mediana, +200 ms intermitente en frío", no "coste cero".
Pendiente para M2: repetir con n ≥ 20 en pieza iluminada y a oscuras, y ver si
el orden de arranque (RGB antes que IR, §2.3) se puede solapar mejor.

## Simultaneidad

RGB 1280x720 MJPG @30 e IR 640x360 @15 transmiten a la vez sin pérdidas: 112
frames RGB en 4 s a 29,6 fps con 0 errores de buffer, mientras el IR entrega
sus 59 frames con 0 metadatos perdidos.

## Sensor de luz ambiente

`/sys/bus/iio/devices/iio:device0` (`als`), legible sin privilegios;
`in_illuminance_raw × in_illuminance_scale` (0,001) = lux. Referencias: pieza
a oscuras 0,4–1,0 lux; luz de tarde 48 lux; noche con lámpara 9 lux.

## LED de la cámara y visibilidad del emisor (observado 2026-09-23)

Observado por el usuario mirando el módulo durante dos capturas de 20 s
(`nirlockctl record --label led-ir --seconds 20` y la misma con `--rgb`;
299 frames IR cada una, 0 metadatos perdidos, la segunda además 586 frames
RGB a 29,6 fps):

| | LED blanco | Emisor IR |
|---|---|---|
| Solo nodo IR (`/dev/video2` + metadatos) | **se enciende** | palpita, visible a simple vista (rojo tenue) |
| IR + RGB de asistencia | se enciende, **sin diferencia perceptible** | igual |

Dos consecuencias para el diseño:

1. **Una ráfaga de escaneo nunca es silenciosa.** El indicador de hardware se
   enciende aunque solo transmita el nodo IR, así que el usuario siempre sabe
   que la cámara está activa, sin depender de nada que dibujemos nosotros.
   Esto responde la pregunta 5 de DESIGN §13: las ráfagas disparadas por
   actividad son aceptables sin indicador propio, y refuerza la elección de
   "solo texto corto" como retroalimentación v1 (pregunta 4) — el indicador
   en pantalla sirve para explicar *por qué falló*, no para avisar que la
   cámara está encendida.
2. **El emisor se ve.** El estrobo es perceptible como un palpitar rojo tenue
   (los LED de 850 nm caen en el borde de la visión humana). De noche eso es
   una señal extra, y también significa que un observador puede notar cuándo
   el equipo está intentando reconocer a alguien.

El LED no distingue IR de RGB, así que no sirve para que el usuario sepa si
el RGB de asistencia está activo; si alguna vez eso importa, tiene que
decirlo la interfaz.

## Tras despertar de una suspensión (E7, medido 2026-09-27)

El experimento E7 estaba abierto desde la Fase 0 y era uno de los riesgos
del diseño: nadie había medido la cámara justo después de un s2idle, y un
reporte externo decía que el emisor quedaba "menos consistente". Medido al
fin, con el caso real (cerrar la tapa, esperar ~15 s, abrirla):

| | Tras despertar | Arranque en frío normal |
|---|---|---|
| Primer frame IR | **145 ms** | 253 ms |
| Decisión K=2 | **512 ms** | ~630 ms |

Es más **rápido**, no más lento: al despertar, el dispositivo USB acaba de
ser reanudado y no está en autosuspensión, así que se ahorra esa espera. No
hubo reenumeración, ni metadatos perdidos, ni emisor errático. El riesgo 5
de DESIGN §12 queda cerrado en el caso que importa (suspensiones cortas por
tapa); las suspensiones largas siguen sin probarse porque este equipo no
despierta de ellas, lo que es un problema anterior y ajeno.


## Otras cámaras: perfiles y cómo contribuir el tuyo

Durante casi todo el desarrollo el daemon llevaba **un** perfil de hardware
compilado dentro, el de arriba. En cualquier otra máquina lo único que sabía
decir era «cámara no encontrada»: cierto e inútil, porque la cámara está ahí y
simplemente nadie la reclamaba. Eso era el bloqueador real para publicar, no
la licencia ni la documentación.

Ahora el perfil es un archivo, y el descubrimiento va al revés: se enumera lo
que la máquina tiene y se busca un perfil que lo reclame.

### Dónde viven los perfiles

| Ruta | Para qué |
|---|---|
| `/etc/nirlock/hw/*.toml` | Los tuyos. Tienen prioridad. |
| `/usr/share/nirlock/hw/*.toml` | Los que vienen con el paquete. |
| compilado | Solo el de referencia, como último recurso para trabajar desde un checkout. |

El primero que reclame un `vendor:product` gana, así que corregir un perfil
del paquete es dejar uno propio en `/etc` — nunca editar el instalado.

Un archivo que no parsea **no se ignora en silencio**: se salta y se registra.
Un perfil que el usuario escribió y que nunca surte efecto es justo el fallo
imposible de diagnosticar desde fuera.

### Qué hace falta para que una cámara sirva

Tres cosas, y las tres son del hardware, no del software:

1. **Un sensor infrarrojo separado**, expuesto como un nodo de captura que
   reporta `GREY`. Una webcam a color normal no sirve: Windows Hello no usa
   la cámara a color, usa el sensor IR de al lado. Si tu portátil no hace
   Windows Hello por rostro en Windows, tampoco lo hará aquí.
2. **El nodo de metadatos `UVCM`** hermano del nodo IR, en la misma interfaz.
   Es lo que trae la etiqueta por frame de iluminación (`MetadataId = 6`,
   `FrameIllumination`). Sin él no hay forma de distinguir un frame iluminado
   por el emisor de uno que no, y con eso se cae todo el argumento contra
   suplantación. Requiere Linux **6.17 o superior**
   (`V4L2_META_FMT_UVC_MSXU_1_5`) y una cámara que exponga la extension unit
   de Microsoft.
3. **Que el emisor estroboscopee solo**, por defecto del firmware. v1 no
   escribe ningún control a la cámara (ADR-0005), así que una cámara que
   necesite que le enciendan el emisor explícitamente no funciona todavía.
   `emitter = "firmware-strobe"` es el único valor aceptado, y eso es
   deliberado: un perfil no puede cambiar de dónde sale la etiqueta.

   **Este es el requisito que más cámaras incumplen**, y es el único que no se
   ve en los descriptores. Por eso `nirlockctl probe` abre el nodo IR dos
   segundos y mira qué hacen las etiquetas de verdad, en vez de suponerlo.
   Hasta el 2026-10-03 lo suponía: escribía `emitter = "firmware-strobe"` como
   literal y le decía a la persona «guarda este perfil y funcionará». En una
   cámara con el emisor apagado eso era falso, y se descubría después de bajar
   286 MB e instalar.

   Lo que puede medir y qué significa cada caso:

   | Medición | Veredicto |
   |---|---|
   | Frames iluminados y oscuros, alternando | Cumple. Es el único caso que genera un perfil |
   | Etiquetas que nunca cambian | Emisor fijo: el bit no aporta información y no sirve |
   | Frames sin etiqueta | El metadato no llega aunque `S_FMT` lo aceptara |
   | Cero frames | El emisor está apagado y el firmware no entrega nada |
   | No se pudo medir | **No es un juicio sobre la cámara**: algo la tenía ocupada |

Lo que **sí** puede variar entre cámaras, y por eso ya no está clavado: la
geometría IR y RGB, los fps, y qué interfaz/índice ocupa cada nodo.

### Cómo generar el tuyo

```
nirlockctl probe
```

Enumera cada cámara USB, dice qué es cada nodo, y para cada una dictamina:
ya soportada, utilizable (y te imprime el perfil), o inutilizable **con el
motivo concreto**. Si sirve, el perfil candidato sale listo para guardar.

Solo el TOML, para redirigirlo:

```
nirlockctl probe --toml > 04f2-b6d0.toml
```

El perfil generado lleva la medición en su campo `notes`, así que la evidencia
de su línea `emitter` viaja con él.

Dos advertencias honestas sobre lo que genera:

- Los **fps son una suposición**. `probe` lee el formato *por defecto* de cada
  nodo, no la lista completa, porque enumerar todos los formatos necesitaría
  `VIDIOC_ENUM_FMT` y ampliar la superficie de ioctls auditada de este crate
  por una herramienta de diagnóstico no vale la pena. Compruébalos con
  `v4l2-ctl -d /dev/videoN --list-formats-ext`.
- El `id` sale del string de producto USB, que suele ser genérico
  (`USB2.0 FHD UVC WebCam`). Ponle el nombre del portátil.

### Enviarlo

Verifícalo de verdad antes: instala el perfil, enrola, y desbloquea. Un perfil
que parsea pero no funciona es peor que ninguno.

Después mándalo como pull request a `hw/`, con el modelo de portátil en el
mensaje. La idea es que la siguiente persona con tu misma cámara no tenga que
volver a averiguarlo.
