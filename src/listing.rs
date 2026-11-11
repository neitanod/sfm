//! Lectura de carpetas y del preview.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: OsString,
    /// Nombre listo para mostrar (UTF-8 con reemplazos si hacía falta).
    pub display: String,
    /// Es carpeta, o un symlink que apunta a una carpeta.
    pub is_dir: bool,
    pub is_link: bool,
    /// A dónde apunta el symlink, listo para mostrar.
    pub link_target: Option<String>,
    /// Symlink que apunta a algo que no existe.
    pub broken: bool,
}

/// Arma una entrada usando el tipo que ya trae `readdir` (d_type), sin hacer
/// stat. Solo los symlinks cuestan un stat y un readlink extra, para saber a
/// qué apuntan.
fn entry_from(de: &fs::DirEntry) -> Entry {
    let name = de.file_name();
    let display = crate::text::sanitize(&name.to_string_lossy());
    let ft = de.file_type().ok();
    let is_link = ft.is_some_and(|t| t.is_symlink());
    let (is_dir, broken, link_target) = if is_link {
        let path = de.path();
        let meta = fs::metadata(&path);
        let target = fs::read_link(&path)
            .ok()
            .map(|t| crate::text::sanitize(&t.to_string_lossy()));
        (meta.as_ref().is_ok_and(|m| m.is_dir()), meta.is_err(), target)
    } else {
        (ft.is_some_and(|t| t.is_dir()), false, None)
    };
    Entry {
        name,
        display,
        is_dir,
        is_link,
        link_target,
        broken,
    }
}

fn is_hidden(name: &OsString) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// Carpetas primero, después por nombre sin distinguir mayúsculas.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| (!e.is_dir, e.display.to_lowercase(), e.name.clone()));
}

/// Lista una carpeta entera, ordenada.
pub fn read_dir(path: &Path, show_hidden: bool, only_dirs: bool) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for de in fs::read_dir(path)? {
        let Ok(de) = de else { continue };
        if !show_hidden && is_hidden(&de.file_name()) {
            continue;
        }
        let e = entry_from(&de);
        if only_dirs && !e.is_dir {
            continue;
        }
        out.push(e);
    }
    sort_entries(&mut out);
    Ok(out)
}

/// Lee como mucho `limit` entradas de una carpeta, en el orden en que las
/// devuelve el sistema de archivos, y después las ordena. Con carpetas de
/// miles de archivos no recorre el resto. El bool dice si quedaron más.
pub fn read_dir_limited(
    path: &Path,
    show_hidden: bool,
    limit: usize,
) -> io::Result<(Vec<Entry>, bool)> {
    let mut out = Vec::with_capacity(limit);
    let mut more = false;
    for de in fs::read_dir(path)? {
        let Ok(de) = de else { continue };
        if !show_hidden && is_hidden(&de.file_name()) {
            continue;
        }
        if out.len() == limit {
            more = true;
            break;
        }
        out.push(entry_from(&de));
    }
    sort_entries(&mut out);
    Ok((out, more))
}

#[derive(Debug)]
pub enum Preview {
    Dir { entries: Vec<Entry>, more: bool },
    Text(Vec<String>),
    Binary { size: u64 },
    Other(String),
    Error(String),
}

/// Bytes que se leen del principio de un archivo para el preview.
const PREVIEW_BYTES: usize = 16 * 1024;

pub fn preview(path: &Path, show_hidden: bool, rows: usize) -> Preview {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => {
            if let Ok(target) = fs::read_link(path) {
                let target = crate::text::sanitize(&target.to_string_lossy());
                return Preview::Error(format!("broken symlink -> {target}"));
            }
            return Preview::Error(e.to_string());
        }
    };
    if meta.is_dir() {
        return match read_dir_limited(path, show_hidden, rows) {
            Ok((entries, more)) => Preview::Dir { entries, more },
            Err(e) => Preview::Error(e.to_string()),
        };
    }
    // Abrir un FIFO o un dispositivo puede bloquear: solo archivos regulares.
    if !meta.is_file() {
        return Preview::Other("special file".into());
    }
    let mut buf = Vec::with_capacity(PREVIEW_BYTES);
    let read = fs::File::open(path)
        .and_then(|f| f.take(PREVIEW_BYTES as u64).read_to_end(&mut buf));
    if let Err(e) = read {
        return Preview::Error(e.to_string());
    }
    text_preview(&buf, rows).unwrap_or(Preview::Binary { size: meta.len() })
}

/// Convierte el principio de un archivo en líneas de texto, o None si parece
/// binario (tiene bytes NUL o UTF-8 inválido antes del corte final).
pub fn text_preview(buf: &[u8], rows: usize) -> Option<Preview> {
    if buf.contains(&0) {
        return None;
    }
    let text = match std::str::from_utf8(buf) {
        Ok(s) => s,
        // El corte a 16 KB puede partir un caracter: eso no lo hace binario.
        Err(e) if e.error_len().is_none() => {
            std::str::from_utf8(&buf[..e.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    let lines = text
        .lines()
        .take(rows)
        .map(crate::text::sanitize)
        .collect();
    Some(Preview::Text(lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sfm-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dirs_first_then_case_insensitive() {
        let d = tmpdir("sort");
        fs::write(d.join("b.txt"), "").unwrap();
        fs::write(d.join("A.txt"), "").unwrap();
        fs::create_dir(d.join("zeta")).unwrap();
        fs::write(d.join(".hidden"), "").unwrap();
        let names: Vec<_> = read_dir(&d, false, false)
            .unwrap()
            .into_iter()
            .map(|e| e.display)
            .collect();
        assert_eq!(names, ["zeta", "A.txt", "b.txt"]);
        assert_eq!(read_dir(&d, true, false).unwrap().len(), 4);
        assert_eq!(read_dir(&d, false, true).unwrap().len(), 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn limited_read_stops_early() {
        let d = tmpdir("limit");
        for i in 0..50 {
            fs::write(d.join(format!("f{i}")), "").unwrap();
        }
        let (entries, more) = read_dir_limited(&d, false, 10).unwrap();
        assert_eq!(entries.len(), 10);
        assert!(more);
        let (entries, more) = read_dir_limited(&d, false, 100).unwrap();
        assert_eq!(entries.len(), 50);
        assert!(!more);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn symlink_to_dir_counts_as_dir() {
        let d = tmpdir("link");
        fs::create_dir(d.join("real")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("alias")).unwrap();
        let e = read_dir(&d, false, false).unwrap();
        let alias = e.iter().find(|e| e.display == "alias").unwrap();
        assert!(alias.is_dir && alias.is_link && !alias.broken);
        assert_eq!(alias.link_target.as_deref(), Some(d.join("real").to_str().unwrap()));
        std::os::unix::fs::symlink("nowhere", d.join("dead")).unwrap();
        let e = read_dir(&d, false, false).unwrap();
        let dead = e.iter().find(|e| e.display == "dead").unwrap();
        assert!(dead.broken && !dead.is_dir);
        assert_eq!(dead.link_target.as_deref(), Some("nowhere"));
        match preview(&d.join("dead"), false, 5) {
            Preview::Error(m) => assert_eq!(m, "broken symlink -> nowhere"),
            other => panic!("{other:?}"),
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn text_and_binary_detection() {
        match text_preview(b"uno\ndos\ntres\n", 2) {
            Some(Preview::Text(l)) => assert_eq!(l, ["uno", "dos"]),
            other => panic!("{other:?}"),
        }
        assert!(text_preview(b"ELF\0\x01", 5).is_none());
        assert!(text_preview(b"\xff\xfe abc", 5).is_none());
        // "ñ" cortada al medio al final del buffer sigue siendo texto
        assert!(text_preview(b"hola \xc3", 5).is_some());
    }

    #[test]
    fn fifo_is_not_opened() {
        let d = tmpdir("fifo");
        let fifo = d.join("pipe");
        let ok = std::process::Command::new("mkfifo").arg(&fifo).status();
        if ok.is_ok_and(|s| s.success()) {
            assert!(matches!(preview(&fifo, false, 10), Preview::Other(_)));
        }
        fs::remove_dir_all(&d).unwrap();
    }
}
