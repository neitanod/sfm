//! Avisos de cambios en disco con inotify, declarado a mano como termios.
//!
//! Se vigilan como mucho tres carpetas: la actual, la padre y la que muestra
//! el preview. Los avisos llegan por un descriptor que el loop principal
//! espera con `poll` junto con el teclado.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

unsafe extern "C" {
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(fd: i32, path: *const u8, mask: u32) -> i32;
    fn inotify_rm_watch(fd: i32, wd: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn close(fd: i32) -> i32;
}

const IN_NONBLOCK: i32 = 0o4000;
const IN_CLOEXEC: i32 = 0o2000000;

const IN_MODIFY: u32 = 0x2;
const IN_ATTRIB: u32 = 0x4;
const IN_CLOSE_WRITE: u32 = 0x8;
const IN_MOVED_FROM: u32 = 0x40;
const IN_MOVED_TO: u32 = 0x80;
const IN_CREATE: u32 = 0x100;
const IN_DELETE: u32 = 0x200;
const IN_DELETE_SELF: u32 = 0x400;
const IN_MOVE_SELF: u32 = 0x800;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_ONLYDIR: u32 = 0x0100_0000;

/// Entradas que aparecen, desaparecen o cambian de nombre.
const STRUCTURE: u32 = IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO;
/// Contenido o permisos de un archivo de la carpeta.
const CONTENT: u32 = IN_MODIFY | IN_CLOSE_WRITE | IN_ATTRIB;
/// La carpeta vigilada misma se borró o se movió.
const SELF_GONE: u32 = IN_DELETE_SELF | IN_MOVE_SELF;

const MASK: u32 = STRUCTURE | CONTENT | SELF_GONE | IN_ONLYDIR;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Current,
    Parent,
    Preview,
}

/// Qué hay que refrescar, juntando todos los avisos leídos.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Cambió la lista de la carpeta actual.
    pub current: bool,
    /// La carpeta actual ya no está donde estaba.
    pub current_gone: bool,
    /// Cambió la lista de carpetas del padre.
    pub parent: bool,
    /// Cambió la carpeta que muestra el preview.
    pub preview: bool,
    /// Archivos de la carpeta actual cuyo contenido o permisos cambiaron.
    pub touched: Vec<OsString>,
    /// Se perdieron avisos: refrescar todo.
    pub overflow: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    /// Suma los avisos de `other` a los que ya estaban esperando.
    pub fn merge(&mut self, other: Changes) {
        self.current |= other.current;
        self.current_gone |= other.current_gone;
        self.parent |= other.parent;
        self.preview |= other.preview;
        self.overflow |= other.overflow;
        for name in other.touched {
            if !self.touched.contains(&name) {
                self.touched.push(name);
            }
        }
    }
}

pub struct Watcher {
    fd: i32,
    watches: Vec<(i32, Role, PathBuf)>,
}

impl Watcher {
    /// None si el kernel no da inotify (límite de instancias, por ejemplo):
    /// sfm sigue andando, sin actualización en vivo.
    pub fn new() -> Option<Watcher> {
        let fd = unsafe { inotify_init1(IN_NONBLOCK | IN_CLOEXEC) };
        (fd >= 0).then(|| Watcher {
            fd,
            watches: Vec::new(),
        })
    }

    pub fn fd(&self) -> i32 {
        self.fd
    }

    /// Ajusta las carpetas vigiladas. Sólo toca las que cambiaron.
    pub fn sync(&mut self, current: &Path, parent: Option<&Path>, preview: Option<&Path>) {
        let wanted = [
            (Role::Current, Some(current)),
            (Role::Parent, parent),
            (Role::Preview, preview),
        ];
        for (role, path) in wanted {
            let have = self.watches.iter().position(|(_, r, _)| *r == role);
            if let (Some(i), Some(p)) = (have, path) {
                if self.watches[i].2 == p {
                    continue;
                }
            }
            if let Some(i) = have {
                let (wd, _, _) = self.watches.remove(i);
                // El mismo wd puede servir a otro rol si dos rutas son la misma
                // carpeta: sólo se suelta cuando nadie más lo usa.
                if !self.watches.iter().any(|(w, _, _)| *w == wd) {
                    unsafe { inotify_rm_watch(self.fd, wd) };
                }
            }
            if let Some(p) = path {
                if let Ok(c) = CString::new(p.as_os_str().as_bytes()) {
                    let wd = unsafe { inotify_add_watch(self.fd, c.as_ptr() as *const u8, MASK) };
                    if wd >= 0 {
                        self.watches.push((wd, role, p.to_path_buf()));
                    }
                }
            }
        }
    }

    /// Lee todos los avisos pendientes sin bloquear.
    pub fn drain(&mut self) -> Changes {
        let mut changes = Changes::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = unsafe { read(self.fd, buf.as_mut_ptr(), buf.len()) };
            if n <= 0 {
                break;
            }
            self.parse(&buf[..n as usize], &mut changes);
        }
        changes
    }

    fn parse(&self, mut buf: &[u8], changes: &mut Changes) {
        // struct inotify_event { int wd; u32 mask; u32 cookie; u32 len; char name[len]; }
        while buf.len() >= 16 {
            let wd = i32::from_ne_bytes(buf[0..4].try_into().unwrap());
            let mask = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
            let len = u32::from_ne_bytes(buf[12..16].try_into().unwrap()) as usize;
            let end = (16 + len).min(buf.len());
            let raw = &buf[16..end];
            let name_len = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            let name = OsString::from_vec(raw[..name_len].to_vec());
            buf = &buf[end..];

            if mask & IN_Q_OVERFLOW != 0 {
                changes.overflow = true;
                continue;
            }
            for (w, role, _) in &self.watches {
                if *w != wd {
                    continue;
                }
                match role {
                    Role::Current => {
                        if mask & STRUCTURE != 0 {
                            changes.current = true;
                        }
                        if mask & SELF_GONE != 0 {
                            changes.current_gone = true;
                        }
                        if mask & CONTENT != 0 && !name.is_empty() && !changes.touched.contains(&name) {
                            changes.touched.push(name.clone());
                        }
                    }
                    Role::Parent => {
                        if mask & (STRUCTURE | SELF_GONE) != 0 {
                            changes.parent = true;
                        }
                    }
                    Role::Preview => {
                        if mask & (STRUCTURE | SELF_GONE) != 0 {
                            changes.preview = true;
                        }
                    }
                }
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sfm-watch-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reports_changes_by_role() {
        let d = tmpdir("roles");
        let cur = d.join("cur");
        let prev = cur.join("sub");
        fs::create_dir_all(&prev).unwrap();
        fs::write(cur.join("f.txt"), "a").unwrap();
        let mut w = Watcher::new().unwrap();
        w.sync(&cur, Some(&d), Some(&prev));
        assert!(w.drain().is_empty());

        fs::write(cur.join("new.txt"), "").unwrap();
        let c = w.drain();
        assert!(c.current && !c.parent && !c.preview, "{c:?}");

        fs::write(cur.join("f.txt"), "changed").unwrap();
        let c = w.drain();
        assert!(!c.current, "{c:?}");
        assert_eq!(c.touched, [OsString::from("f.txt")]);

        fs::create_dir(d.join("sibling")).unwrap();
        assert!(w.drain().parent);

        fs::write(prev.join("inside"), "").unwrap();
        assert!(w.drain().preview);

        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn current_gone_and_resync() {
        let d = tmpdir("gone");
        let cur = d.join("cur");
        fs::create_dir_all(&cur).unwrap();
        let mut w = Watcher::new().unwrap();
        w.sync(&cur, Some(&d), None);
        fs::remove_dir(&cur).unwrap();
        assert!(w.drain().current_gone);

        // Tras cambiar de carpeta, la vieja deja de avisar.
        let other = d.join("other");
        fs::create_dir(&other).unwrap();
        w.sync(&d, None, None);
        w.drain();
        fs::write(other.join("x"), "").unwrap();
        assert!(w.drain().is_empty());
        fs::remove_dir_all(&d).unwrap();
    }
}
