# sfm

[Read in English](README.md)

Un file manager de terminal en tres columnas, escrito en Rust **sin
dependencias**: sólo la biblioteca estándar. Sale como un único binario
estático que se copia a cualquier Linux x86_64 y anda.

```
 /home/yo/proyectos/sfm
 notas/          │ src/                          │ fn main() {
 proyectos/      │ target/                       │     println!("hola");
 sfm/            │ Cargo.toml                    │ }
                 │ README.md                     │
 3/4  -rw-r--r--  1.2K               ←↑↓→ move  c copy  p paste  m move  e edit  . hidden  q quit
```

- **Izquierda:** las carpetas de la carpeta padre, con la actual resaltada.
- **Centro:** la carpeta actual. Acá está el cursor.
- **Derecha:** el preview de lo que está bajo el cursor. Una carpeta muestra
  sus primeras entradas (deja de leer al llenar la pantalla, así que una
  carpeta con un millón de archivos cuesta lo mismo que una con diez). Un
  archivo de texto muestra sus primeras líneas. Los binarios y los archivos
  especiales (FIFOs, dispositivos) no se abren nunca.

## Teclas

| Tecla | Acción |
|---|---|
| `↑` `↓` (`k` `j`) | Mover el cursor |
| `←` (`h`, Backspace) | Ir a la carpeta padre (`cd ..`) |
| `→` (`l`, Enter) | Entrar a la carpeta bajo el cursor |
| RePág AvPág Inicio Fin (`g` `G`) | Saltar |
| `c` | Marcar el archivo o la carpeta bajo el cursor |
| `p` | Pegar acá una copia de lo marcado (`cp -a`), con confirmación y/N |
| `m` | Mover acá lo marcado (`mv`), con confirmación y/N |
| `e` | Editar el archivo bajo el cursor con `$VISUAL`, `$EDITOR`, `vim` o `vi` |
| `.` | Mostrar u ocultar los archivos que empiezan con punto |
| `r` | Recargar |
| `q` Esc Ctrl-C | Salir |

`p` y `m` nunca pisan nada: si en la carpeta actual ya hay algo con el mismo
nombre, se niegan y lo dicen. Una carpeta no se puede pegar ni mover dentro de
sí misma.

## Instalación

Bajá el binario estático de la
[página de releases](https://github.com/neitanod/sfm/releases) y ponelo en el
`PATH`:

```sh
chmod +x sfm && sudo mv sfm /usr/local/bin/
scp sfm usuario@servidor:/usr/local/bin/   # no tiene dependencias en runtime
```

O compilalo:

```sh
cargo build --release                                   # dinámico, usa glibc
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl  # totalmente estático
```

## Uso

```sh
sfm            # arranca en la carpeta actual
sfm /var/log   # arranca en otra
```

## Límites

- Sólo Linux. La terminal se maneja con `termios` e `ioctl` declarados a mano,
  con el layout y las constantes de Linux.
- El preview de una carpeta lee las primeras entradas en el orden en que las
  devuelve el sistema de archivos y ordena sólo esas: en una carpeta enorme
  muestra una muestra, y no el principio alfabético.
- El ancho de los caracteres es aproximado (CJK y emoji cuentan dos columnas).
- Copiar y mover son sincrónicos: una copia enorme congela la pantalla hasta
  que termina.

## Licencia

MIT
