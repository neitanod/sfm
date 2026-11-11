//! Terminal en modo crudo sin crates: termios e ioctl declarados a mano.
//!
//! Las constantes y el layout de `Termios` son los de Linux (x86_64 y aarch64),
//! iguales con glibc y con musl.

use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::sync::OnceLock;

#[cfg(not(target_os = "linux"))]
compile_error!("sfm solo soporta Linux: el layout de termios está declarado a mano");

#[repr(C)]
#[derive(Clone, Copy)]
struct Termios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_line: u8,
    c_cc: [u8; 32],
    c_ispeed: u32,
    c_ospeed: u32,
}

#[repr(C)]
#[derive(Default)]
struct Winsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

unsafe extern "C" {
    fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, action: i32, termios: *const Termios) -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn poll(fds: *mut PollFd, nfds: u64, timeout: i32) -> i32;
}

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x1;

const STDIN: i32 = 0;
const STDOUT: i32 = 1;
const TCSAFLUSH: i32 = 2;
const TIOCGWINSZ: u64 = 0x5413;

// c_iflag
const BRKINT: u32 = 0o2;
const INPCK: u32 = 0o20;
const ISTRIP: u32 = 0o40;
const ICRNL: u32 = 0o400;
const IXON: u32 = 0o2000;
// c_oflag
const OPOST: u32 = 0o1;
// c_cflag
const CS8: u32 = 0o60;
// c_lflag
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const IEXTEN: u32 = 0o100000;
// c_cc
const VTIME: usize = 5;
const VMIN: usize = 6;

/// Estado original de la terminal, para restaurarlo al salir o en un panic.
static ORIGINAL: OnceLock<Termios> = OnceLock::new();
/// Estado crudo, para volver a él después de abrir el editor.
static RAW: OnceLock<Termios> = OnceLock::new();

const ENTER_SCREEN: &str = "\x1b[?1049h\x1b[?25l\x1b[2J";
const LEAVE_SCREEN: &str = "\x1b[0m\x1b[?25h\x1b[?1049l";

/// Pasa la terminal a modo crudo y a la pantalla alternativa.
pub fn enter() -> io::Result<()> {
    let mut t = unsafe { std::mem::zeroed::<Termios>() };
    if unsafe { tcgetattr(STDIN, &mut t) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "stdin is not a terminal",
        ));
    }
    let _ = ORIGINAL.set(t);

    let mut raw = t;
    raw.c_iflag &= !(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    raw.c_oflag &= !OPOST;
    raw.c_cflag |= CS8;
    raw.c_lflag &= !(ECHO | ICANON | IEXTEN | ISIG);
    // read() vuelve en el acto con lo que haya: la espera la hace wait().
    raw.c_cc[VMIN] = 0;
    raw.c_cc[VTIME] = 0;
    let _ = RAW.set(raw);

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave();
        prev(info);
    }));
    resume()
}

/// Vuelve al modo crudo y a la pantalla alternativa (después de `leave`).
pub fn resume() -> io::Result<()> {
    if let Some(raw) = RAW.get() {
        if unsafe { tcsetattr(STDIN, TCSAFLUSH, raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut out = io::stdout();
    out.write_all(ENTER_SCREEN.as_bytes())?;
    out.flush()
}

/// Restaura la terminal. Se puede llamar más de una vez.
pub fn leave() {
    if let Some(t) = ORIGINAL.get() {
        unsafe { tcsetattr(STDIN, TCSAFLUSH, t) };
    }
    let mut out = io::stdout();
    let _ = out.write_all(LEAVE_SCREEN.as_bytes());
    let _ = out.flush();
}

/// Tamaño de la terminal en (filas, columnas).
pub fn size() -> (usize, usize) {
    let mut ws = Winsize::default();
    let ok = unsafe { ioctl(STDOUT, TIOCGWINSZ, &mut ws as *mut Winsize) } == 0;
    if ok && ws.ws_row > 0 && ws.ws_col > 0 {
        (ws.ws_row as usize, ws.ws_col as usize)
    } else {
        (24, 80)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Ctrl(u8),
    Char(char),
}

/// Espera hasta `timeout_ms` a que haya teclas en stdin o datos en `extra`.
/// Devuelve (hay teclas, hay datos en extra). El timeout corto es lo que
/// permite notar el cambio de tamaño de la ventana sin manejar señales.
pub fn wait(extra: Option<i32>, timeout_ms: i32) -> (bool, bool) {
    let mut fds = [
        PollFd {
            fd: STDIN,
            events: POLLIN,
            revents: 0,
        },
        PollFd {
            fd: extra.unwrap_or(-1),
            events: POLLIN,
            revents: 0,
        },
    ];
    let n = if extra.is_some() { 2 } else { 1 };
    if unsafe { poll(fds.as_mut_ptr(), n, timeout_ms) } <= 0 {
        return (false, false);
    }
    (fds[0].revents != 0, fds[1].revents != 0)
}

/// Lee lo que haya en stdin sin esperar y lo traduce a teclas.
pub fn read_keys() -> io::Result<Vec<Key>> {
    let mut stdin = ManuallyDrop::new(unsafe { File::from_raw_fd(STDIN) });
    let mut buf = [0u8; 256];
    let n = match stdin.read(&mut buf) {
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::Interrupted => 0,
        Err(e) => return Err(e),
    };
    Ok(parse_keys(&buf[..n]))
}

/// Traduce bytes crudos de la terminal a teclas. Puede venir más de una
/// tecla por lectura cuando se mantiene apretada una flecha.
pub fn parse_keys(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            let (key, used) = parse_escape(&bytes[i..]);
            keys.push(key);
            i += used;
            continue;
        }
        let key = match b {
            b'\r' | b'\n' => Key::Enter,
            0x01..=0x1a => Key::Ctrl(b'a' + b - 1),
            _ => {
                // Decodificar un caracter UTF-8 completo.
                let len = utf8_len(b);
                let end = (i + len).min(bytes.len());
                let ch = std::str::from_utf8(&bytes[i..end])
                    .ok()
                    .and_then(|s| s.chars().next());
                i = end;
                if let Some(c) = ch {
                    keys.push(Key::Char(c));
                }
                continue;
            }
        };
        keys.push(key);
        i += 1;
    }
    keys
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Interpreta una secuencia que empieza con ESC. Devuelve la tecla y cuántos
/// bytes consumió.
fn parse_escape(s: &[u8]) -> (Key, usize) {
    if s.len() < 2 {
        return (Key::Esc, 1);
    }
    match s[1] {
        // ESC O x: flechas en modo aplicación
        b'O' if s.len() >= 3 => {
            let key = match s[2] {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                _ => Key::Esc,
            };
            (key, 3)
        }
        b'[' => {
            // CSI: parámetros hasta el byte final (0x40..=0x7e)
            let mut j = 2;
            while j < s.len() && !(0x40..=0x7e).contains(&s[j]) {
                j += 1;
            }
            if j >= s.len() {
                return (Key::Esc, s.len());
            }
            let params = &s[2..j];
            let key = match s[j] {
                b'A' => Key::Up,
                b'B' => Key::Down,
                b'C' => Key::Right,
                b'D' => Key::Left,
                b'H' => Key::Home,
                b'F' => Key::End,
                b'~' => match params {
                    b"1" | b"7" => Key::Home,
                    b"4" | b"8" => Key::End,
                    b"5" => Key::PageUp,
                    b"6" => Key::PageDown,
                    _ => Key::Esc,
                },
                _ => Key::Esc,
            };
            (key, j + 1)
        }
        // ESC seguido de otra cosa: Alt+tecla. Lo tomamos como ESC solo.
        _ => (Key::Esc, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_in_both_modes() {
        assert_eq!(
            parse_keys(b"\x1b[A\x1b[B\x1bOC\x1bOD"),
            vec![Key::Up, Key::Down, Key::Right, Key::Left]
        );
    }

    #[test]
    fn page_and_home_keys() {
        assert_eq!(
            parse_keys(b"\x1b[5~\x1b[6~\x1b[H\x1b[4~"),
            vec![Key::PageUp, Key::PageDown, Key::Home, Key::End]
        );
    }

    #[test]
    fn lone_escape_and_chars() {
        assert_eq!(parse_keys(b"\x1b"), vec![Key::Esc]);
        assert_eq!(parse_keys(b"q\r"), vec![Key::Char('q'), Key::Enter]);
        assert_eq!(parse_keys(&[3]), vec![Key::Ctrl(b'c')]);
        assert_eq!(parse_keys("ñ".as_bytes()), vec![Key::Char('ñ')]);
    }

    #[test]
    fn repeated_arrows_in_one_read() {
        assert_eq!(parse_keys(b"\x1b[B\x1b[B\x1b[B").len(), 3);
    }

    #[test]
    fn termios_layout_matches_linux() {
        assert_eq!(std::mem::size_of::<Termios>(), 60);
    }
}
