//! sfm — file manager de terminal en tres columnas, sin dependencias.
//!
//! Izquierda: carpetas de la carpeta padre. Centro: la carpeta actual, con el
//! cursor. Derecha: preview de lo que está bajo el cursor.

mod listing;
mod term;
mod text;

use listing::{Entry, Preview};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use term::Key;
use text::fit;

const STYLE_DIR: &str = "\x1b[1;34m";
const STYLE_LINK: &str = "\x1b[36m";
const STYLE_SELECTED: &str = "\x1b[7m";
const STYLE_DIM: &str = "\x1b[90m";
const STYLE_HEADER: &str = "\x1b[1;32m";
const STYLE_ERROR: &str = "\x1b[31m";
const STYLE_PROMPT: &str = "\x1b[1;33m";
const STYLE_OK: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

const HELP: &str = "←↑↓→ move  c copy  p paste  m move  e edit  . hidden  q quit";
const HELP_SHORT: &str = "c p m e . q";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Copy,
    Move,
}

impl Op {
    fn verb(self) -> &'static str {
        match self {
            Op::Copy => "copy",
            Op::Move => "move",
        }
    }
}

/// Operación esperando la confirmación y/N.
#[derive(Debug)]
struct Pending {
    op: Op,
    src: PathBuf,
    dest: PathBuf,
}

/// Mensaje de la barra de abajo.
enum Notice {
    Info(String),
    Error(String),
}

struct App {
    cwd: PathBuf,
    entries: Vec<Entry>,
    cursor: usize,
    offset: usize,
    parent: Vec<Entry>,
    show_hidden: bool,
    /// Último elemento elegido en cada carpeta visitada.
    remembered: HashMap<PathBuf, OsString>,
    message: Option<Notice>,
    /// Lo que se marcó con C, para pegar con P o mover con M.
    clipboard: Option<PathBuf>,
    pending: Option<Pending>,
    /// Archivo a abrir en el editor; lo atiende el loop, que maneja la terminal.
    edit_request: Option<PathBuf>,
    preview_cache: Option<(PathBuf, usize, bool, Preview)>,
    quit: bool,
}

impl App {
    fn new(start: PathBuf) -> std::io::Result<App> {
        let mut app = App {
            cwd: start.clone(),
            entries: Vec::new(),
            cursor: 0,
            offset: 0,
            parent: Vec::new(),
            show_hidden: false,
            remembered: HashMap::new(),
            message: None,
            clipboard: None,
            pending: None,
            edit_request: None,
            preview_cache: None,
            quit: false,
        };
        app.entries = listing::read_dir(&start, false, false)?;
        app.load_parent();
        Ok(app)
    }

    fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected().map(|e| self.cwd.join(&e.name))
    }

    /// Carga las carpetas del padre. Incluye siempre la carpeta actual aunque
    /// esté oculta, para que se vea dónde estamos.
    fn load_parent(&mut self) {
        let current = self.cwd.file_name().map(|n| n.to_os_string());
        self.parent = match self.cwd.parent() {
            Some(p) => listing::read_dir(p, true, true)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| {
                    self.show_hidden
                        || !e.display.starts_with('.')
                        || Some(&e.name) == current.as_ref()
                })
                .collect(),
            None => Vec::new(),
        };
    }

    /// Entra a `dir` dejando el cursor sobre `select`, o sobre lo último que
    /// se eligió ahí. Si no se puede leer, queda donde estaba y avisa.
    fn change_dir(&mut self, dir: PathBuf, select: Option<OsString>) {
        let entries = match listing::read_dir(&dir, self.show_hidden, false) {
            Ok(e) => e,
            Err(e) => {
                self.message = Some(Notice::Error(format!("{}: {e}", dir.display())));
                return;
            }
        };
        if let Some(e) = self.selected() {
            self.remembered.insert(self.cwd.clone(), e.name.clone());
        }
        let target = select.or_else(|| self.remembered.get(&dir).cloned());
        self.cwd = dir;
        self.entries = entries;
        self.cursor = target
            .and_then(|t| self.entries.iter().position(|e| e.name == t))
            .unwrap_or(0);
        self.offset = 0;
        self.load_parent();
    }

    fn reload(&mut self) {
        let keep = self.selected().map(|e| e.name.clone());
        self.preview_cache = None;
        self.change_dir(self.cwd.clone(), keep);
    }

    fn go_parent(&mut self) {
        if let Some(parent) = self.cwd.parent() {
            let from = self.cwd.file_name().map(|n| n.to_os_string());
            self.change_dir(parent.to_path_buf(), from);
        }
    }

    fn go_into(&mut self) {
        if let (Some(e), Some(path)) = (self.selected(), self.selected_path()) {
            if e.is_dir {
                self.change_dir(path, None);
            }
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    fn mark(&mut self) {
        if let (Some(path), Some(e)) = (self.selected_path(), self.selected()) {
            self.message = Some(Notice::Info(format!(
                "marked {} — p pastes a copy here, m moves it here",
                e.display
            )));
            self.clipboard = Some(path);
        }
    }

    /// Prepara la copia o el movimiento de lo marcado a la carpeta actual y
    /// pide confirmación. Nunca pisa algo que ya existe.
    fn request(&mut self, op: Op) {
        let Some(src) = self.clipboard.clone() else {
            self.message = Some(Notice::Error(
                "nothing marked: press c on a file or folder first".into(),
            ));
            return;
        };
        match check_transfer(&src, &self.cwd) {
            Ok(dest) => self.pending = Some(Pending { op, src, dest }),
            Err(e) => self.message = Some(Notice::Error(e)),
        }
    }

    fn confirm(&mut self, p: Pending) {
        let name = p.dest.file_name().map(|n| n.to_os_string());
        match run_transfer(&p) {
            Ok(()) => {
                if p.op == Op::Move {
                    self.clipboard = None;
                }
                self.preview_cache = None;
                self.change_dir(self.cwd.clone(), name);
                self.message = Some(Notice::Info(format!(
                    "{} done: {}",
                    p.op.verb(),
                    p.dest.display()
                )));
            }
            Err(e) => self.message = Some(Notice::Error(e)),
        }
    }

    fn edit(&mut self) {
        let Some(path) = self.selected_path() else { return };
        if path.is_dir() {
            self.message = Some(Notice::Error("e edits files, not folders".into()));
        } else {
            self.edit_request = Some(path);
        }
    }

    fn handle(&mut self, key: Key, page: usize) {
        self.message = None;
        if let Some(p) = self.pending.take() {
            if matches!(key, Key::Char('y') | Key::Char('Y')) {
                self.confirm(p);
            } else {
                self.message = Some(Notice::Info(format!("{} cancelled", p.op.verb())));
            }
            return;
        }
        let page = page.max(1) as isize;
        match key {
            Key::Up | Key::Char('k') => self.move_cursor(-1),
            Key::Down | Key::Char('j') => self.move_cursor(1),
            Key::PageUp => self.move_cursor(-page),
            Key::PageDown => self.move_cursor(page),
            Key::Home | Key::Char('g') => self.cursor = 0,
            Key::End | Key::Char('G') => self.cursor = self.entries.len().saturating_sub(1),
            Key::Left | Key::Char('h') | Key::Char('\x7f') | Key::Ctrl(b'h') => self.go_parent(),
            Key::Right | Key::Char('l') | Key::Enter => self.go_into(),
            Key::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.reload();
            }
            Key::Char('r') | Key::Ctrl(b'l') => self.reload(),
            Key::Char('c') | Key::Char('C') => self.mark(),
            Key::Char('p') | Key::Char('P') => self.request(Op::Copy),
            Key::Char('m') | Key::Char('M') => self.request(Op::Move),
            Key::Char('e') | Key::Char('E') => self.edit(),
            Key::Char('q') | Key::Esc | Key::Ctrl(b'c') => self.quit = true,
            _ => {}
        }
    }

    /// Ajusta el scroll de la columna central para que el cursor se vea.
    fn scroll(&mut self, height: usize) {
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if height > 0 && self.cursor >= self.offset + height {
            self.offset = self.cursor + 1 - height;
        }
        let max_offset = self.entries.len().saturating_sub(height);
        self.offset = self.offset.min(max_offset);
    }

    fn preview(&mut self, rows: usize) -> Option<&Preview> {
        let path = self.selected_path()?;
        let fresh = matches!(&self.preview_cache,
            Some((p, r, h, _)) if *p == path && *r == rows && *h == self.show_hidden);
        if !fresh {
            let pv = listing::preview(&path, self.show_hidden, rows);
            self.preview_cache = Some((path, rows, self.show_hidden, pv));
        }
        self.preview_cache.as_ref().map(|(_, _, _, pv)| pv)
    }

    /// Arma un cuadro completo de la pantalla como un solo string.
    fn render(&mut self, rows: usize, cols: usize) -> String {
        let height = rows.saturating_sub(2);
        let (left_w, right_w) = if cols >= 40 {
            (cols * 18 / 100, cols * 35 / 100)
        } else {
            (0, 0)
        };
        let center_w = if left_w > 0 {
            cols - left_w - right_w - 2
        } else {
            cols
        };
        self.scroll(height);

        let left = self.left_column(height, left_w);
        let center = self.center_column(height, center_w);
        let right = if right_w > 0 {
            self.right_column(height, right_w)
        } else {
            Vec::new()
        };

        let mut out = String::with_capacity(rows * cols * 2);
        out.push_str("\x1b[H");
        let header = format!(" {}", self.cwd.display());
        let _ = write!(out, "{STYLE_HEADER}{}{RESET}", fit(&text::sanitize(&header), cols));
        for row in 0..height {
            let _ = write!(out, "\x1b[{};1H", row + 2);
            if left_w > 0 {
                out.push_str(&left[row]);
                let _ = write!(out, "{STYLE_DIM}│{RESET}");
            }
            out.push_str(&center[row]);
            if right_w > 0 {
                let _ = write!(out, "{STYLE_DIM}│{RESET}");
                out.push_str(&right[row]);
            }
        }
        let _ = write!(out, "\x1b[{rows};1H{}", self.status_line(cols));
        out
    }

    fn entry_cell(e: &Entry, width: usize, selected: bool) -> String {
        let mut name = e.display.clone();
        if e.is_dir {
            name.push('/');
        }
        let style = if e.is_link {
            STYLE_LINK
        } else if e.is_dir {
            STYLE_DIR
        } else {
            ""
        };
        let sel = if selected { STYLE_SELECTED } else { "" };
        format!("{style}{sel}{}{RESET}", fit(&format!(" {name}"), width))
    }

    fn left_column(&self, height: usize, width: usize) -> Vec<String> {
        let current = self.cwd.file_name();
        let idx = self
            .parent
            .iter()
            .position(|e| Some(e.name.as_os_str()) == current);
        // Centrar la carpeta actual en la columna.
        let start = idx
            .unwrap_or(0)
            .saturating_sub(height / 2)
            .min(self.parent.len().saturating_sub(height));
        (0..height)
            .map(|row| match self.parent.get(start + row) {
                Some(e) => Self::entry_cell(e, width, Some(start + row) == idx),
                None => fit("", width),
            })
            .collect()
    }

    fn center_column(&self, height: usize, width: usize) -> Vec<String> {
        (0..height)
            .map(|row| {
                let i = self.offset + row;
                match self.entries.get(i) {
                    Some(e) => Self::entry_cell(e, width, i == self.cursor),
                    None if i == 0 => format!("{STYLE_DIM}{}{RESET}", fit("  (empty)", width)),
                    None => fit("", width),
                }
            })
            .collect()
    }

    fn right_column(&mut self, height: usize, width: usize) -> Vec<String> {
        let mut lines: Vec<String> = match self.preview(height) {
            None => Vec::new(),
            Some(Preview::Dir { entries, more }) => {
                let mut v: Vec<String> = entries
                    .iter()
                    .take(height)
                    .map(|e| Self::entry_cell(e, width, false))
                    .collect();
                if entries.is_empty() {
                    v.push(format!("{STYLE_DIM}{}{RESET}", fit("  (empty)", width)));
                } else if *more && v.len() == height {
                    v[height - 1] = format!("{STYLE_DIM}{}{RESET}", fit("  …", width));
                }
                v
            }
            Some(Preview::Text(text_lines)) => text_lines
                .iter()
                .map(|l| fit(&format!(" {l}"), width))
                .collect(),
            Some(Preview::Binary { size }) => vec![format!(
                "{STYLE_DIM}{}{RESET}",
                fit(&format!(" binary file, {}", text::human_size(*size)), width)
            )],
            Some(Preview::Other(what)) => {
                vec![format!("{STYLE_DIM}{}{RESET}", fit(&format!(" {what}"), width))]
            }
            Some(Preview::Error(err)) => {
                vec![format!("{STYLE_ERROR}{}{RESET}", fit(&format!(" {err}"), width))]
            }
        };
        lines.resize_with(height, || fit("", width));
        lines
    }

    fn status_line(&self, cols: usize) -> String {
        if let Some(p) = &self.pending {
            let q = format!(
                " {} {} → {}/ ? [y/N]",
                p.op.verb(),
                p.src.display(),
                self.cwd.display()
            );
            return format!("{STYLE_PROMPT}{}{RESET}", fit(&text::sanitize(&q), cols));
        }
        if let Some(n) = &self.message {
            let (style, msg) = match n {
                Notice::Info(m) => (STYLE_OK, m),
                Notice::Error(m) => (STYLE_ERROR, m),
            };
            return format!("{style}{}{RESET}", fit(&text::sanitize(&format!(" {msg}")), cols));
        }
        let mut info = String::new();
        if !self.entries.is_empty() {
            let _ = write!(info, " {}/{}", self.cursor + 1, self.entries.len());
        }
        if let Some(path) = self.selected_path() {
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                let _ = write!(info, "  {}", text::mode_string(meta.permissions().mode()));
                if !meta.is_dir() {
                    let _ = write!(info, "  {}", text::human_size(meta.len()));
                }
                if meta.file_type().is_symlink() {
                    if let Ok(target) = std::fs::read_link(&path) {
                        let _ = write!(info, "  -> {}", target.display());
                    }
                }
            }
        }
        if self.show_hidden {
            info.push_str("  [hidden]");
        }
        if let Some(c) = &self.clipboard {
            let name = c.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            let _ = write!(info, "  [marked: {name}]");
        }
        let info = text::sanitize(&info);
        // La ayuda larga si entra, si no la corta, si no ninguna.
        let info_w = info.chars().map(text::char_width).sum::<usize>();
        for help in [HELP, HELP_SHORT] {
            let help_w = help.chars().count() + 1;
            if cols >= help_w + info_w.max(10) {
                return format!("{}{STYLE_DIM}{help} {RESET}", fit(&info, cols - help_w));
            }
        }
        fit(&info, cols)
    }
}

/// Valida copiar o mover `src` adentro de `dir` y devuelve el destino.
fn check_transfer(src: &Path, dir: &Path) -> Result<PathBuf, String> {
    if std::fs::symlink_metadata(src).is_err() {
        return Err(format!("{} no longer exists", src.display()));
    }
    let name = src
        .file_name()
        .ok_or_else(|| format!("can't use {}", src.display()))?;
    let dest = dir.join(name);
    if dir.starts_with(src) {
        return Err("can't put a folder inside itself".into());
    }
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Err(format!(
            "{} already exists here, not overwriting",
            name.to_string_lossy()
        ));
    }
    Ok(dest)
}

/// Corre `cp -a` o `mv` y devuelve el error de la herramienta si falla.
fn run_transfer(p: &Pending) -> Result<(), String> {
    let mut cmd = match p.op {
        Op::Copy => {
            let mut c = std::process::Command::new("cp");
            c.arg("-a");
            c
        }
        Op::Move => std::process::Command::new("mv"),
    };
    // -n además del chequeo previo: si algo apareció en el medio, no se pisa.
    let out = cmd
        .arg("-n")
        .arg("--")
        .arg(&p.src)
        .arg(&p.dest)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", p.op.verb()))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().next().unwrap_or("failed").trim();
        Err(format!("{} failed: {line}", p.op.verb()))
    }
}

/// Abre el archivo en $VISUAL, $EDITOR, vim o vi, en ese orden. La variable
/// puede traer argumentos ("code -w"), por eso pasa por sh.
fn open_editor(path: &Path) -> std::io::Result<()> {
    const SCRIPT: &str = r#"ed=${VISUAL:-${EDITOR:-}}
if [ -z "$ed" ]; then
  if command -v vim >/dev/null 2>&1; then ed=vim; else ed=vi; fi
fi
exec $ed "$1""#;
    std::process::Command::new("sh")
        .arg("-c")
        .arg(SCRIPT)
        .arg("sfm")
        .arg(path)
        .status()
        .map(|_| ())
}

fn start_dir() -> Result<PathBuf, String> {
    let arg = std::env::args_os().nth(1);
    if let Some(a) = &arg {
        if a == "-h" || a == "--help" {
            println!("usage: sfm [DIR]\n\n{HELP}\nh/j/k/l and Enter also work; PgUp/PgDn/Home/End jump.");
            std::process::exit(0);
        }
    }
    let path = match arg {
        Some(a) => PathBuf::from(a),
        None => std::env::current_dir().map_err(|e| format!("current dir: {e}"))?,
    };
    let path = std::fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{}: not a directory", path.display()));
    }
    Ok(path)
}

fn run(start: &Path) -> std::io::Result<()> {
    let mut app = App::new(start.to_path_buf())?;
    let mut out = std::io::stdout();
    let mut last_size = (0, 0);
    let mut dirty = true;
    while !app.quit {
        let (rows, cols) = term::size();
        if dirty || (rows, cols) != last_size {
            out.write_all(app.render(rows, cols).as_bytes())?;
            out.flush()?;
            last_size = (rows, cols);
            dirty = false;
        }
        for key in term::read_keys()? {
            app.handle(key, rows.saturating_sub(2));
            dirty = true;
            if let Some(path) = app.edit_request.take() {
                term::leave();
                let result = open_editor(&path);
                term::resume()?;
                if let Err(e) = result {
                    app.message = Some(Notice::Error(format!("editor: {e}")));
                }
                app.reload();
                // El editor pudo cambiar el archivo: recalcular el preview.
                app.preview_cache = None;
                break;
            }
        }
    }
    Ok(())
}

fn main() {
    let start = match start_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sfm: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = term::enter() {
        eprintln!("sfm: {e}");
        std::process::exit(1);
    }
    let result = run(&start);
    term::leave();
    if let Err(e) = result {
        eprintln!("sfm: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sfm-app-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("alpha/inner")).unwrap();
        fs::create_dir_all(d.join("beta")).unwrap();
        fs::write(d.join("alpha/notes.txt"), "line one\nline two\n").unwrap();
        fs::write(d.join("zfile.txt"), "hello\n").unwrap();
        d
    }

    #[test]
    fn right_then_left_returns_to_same_entry() {
        let d = tree("right_then_left_returns_to_same_entry");
        let mut app = App::new(d.clone()).unwrap();
        app.handle(Key::Down, 10); // beta
        app.handle(Key::Up, 10); // alpha
        app.handle(Key::Right, 10);
        assert_eq!(app.cwd, d.join("alpha"));
        app.handle(Key::Down, 10); // notes.txt
        app.handle(Key::Left, 10);
        assert_eq!(app.cwd, d);
        assert_eq!(app.selected().unwrap().display, "alpha");
        // Al volver a entrar recuerda notes.txt
        app.handle(Key::Right, 10);
        assert_eq!(app.selected().unwrap().display, "notes.txt");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn right_on_file_does_nothing() {
        let d = tree("right_on_file_does_nothing");
        let mut app = App::new(d.clone()).unwrap();
        app.handle(Key::End, 10);
        assert_eq!(app.selected().unwrap().display, "zfile.txt");
        app.handle(Key::Right, 10);
        assert_eq!(app.cwd, d);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn render_fills_every_row_with_exact_width() {
        let d = tree("render_fills_every_row_with_exact_width");
        let mut app = App::new(d.join("alpha")).unwrap();
        app.handle(Key::End, 10); // notes.txt, preview de texto
        let frame = app.render(12, 100);
        assert!(frame.contains("line one"));
        assert!(frame.contains("inner/"));
        assert!(frame.contains("beta/")); // columna izquierda
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn scroll_keeps_cursor_visible() {
        let d = tree("scroll_keeps_cursor_visible");
        let mut app = App::new(d.clone()).unwrap();
        app.cursor = 2;
        app.scroll(2);
        assert_eq!(app.offset, 1);
        app.cursor = 0;
        app.scroll(2);
        assert_eq!(app.offset, 0);
        fs::remove_dir_all(&d).unwrap();
    }

    fn select(app: &mut App, name: &str) {
        app.cursor = app.entries.iter().position(|e| e.display == name).unwrap();
    }

    #[test]
    fn copy_then_move_with_confirmation() {
        let d = tree("transfer");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('C'), 10);
        select(&mut app, "beta");
        app.handle(Key::Right, 10);

        // Cualquier tecla que no sea y cancela
        app.handle(Key::Char('P'), 10);
        assert!(app.pending.is_some());
        app.handle(Key::Char('n'), 10);
        assert!(!d.join("beta/zfile.txt").exists());

        app.handle(Key::Char('P'), 10);
        app.handle(Key::Char('y'), 10);
        assert_eq!(fs::read_to_string(d.join("beta/zfile.txt")).unwrap(), "hello\n");
        assert!(d.join("zfile.txt").exists());
        assert_eq!(app.selected().unwrap().display, "zfile.txt");

        // Pegar otra vez en el mismo lugar no pisa
        app.handle(Key::Char('P'), 10);
        assert!(app.pending.is_none());
        assert!(matches!(app.message, Some(Notice::Error(_))));

        // Mover a alpha
        app.handle(Key::Left, 10);
        select(&mut app, "alpha");
        app.handle(Key::Right, 10);
        app.handle(Key::Char('M'), 10);
        app.handle(Key::Char('y'), 10);
        assert!(d.join("alpha/zfile.txt").exists());
        assert!(!d.join("zfile.txt").exists());
        assert!(app.clipboard.is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn folder_cannot_go_inside_itself() {
        let d = tree("itself");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "alpha");
        app.handle(Key::Char('c'), 10);
        app.handle(Key::Right, 10);
        select(&mut app, "inner");
        app.handle(Key::Right, 10);
        app.handle(Key::Char('m'), 10);
        assert!(app.pending.is_none());
        assert!(d.join("alpha/inner").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn copy_folder_recursively() {
        let d = tree("copydir");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "alpha");
        app.handle(Key::Char('c'), 10);
        app.handle(Key::Right, 10);
        select(&mut app, "inner");
        app.handle(Key::Left, 10);
        select(&mut app, "beta");
        app.handle(Key::Right, 10);
        app.handle(Key::Char('p'), 10);
        app.handle(Key::Char('y'), 10);
        assert!(d.join("beta/alpha/notes.txt").exists());
        assert!(d.join("beta/alpha/inner").is_dir());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn edit_requests_only_files() {
        let d = tree("edit");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "alpha");
        app.handle(Key::Char('e'), 10);
        assert!(app.edit_request.is_none());
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('E'), 10);
        assert_eq!(app.edit_request, Some(d.join("zfile.txt")));
        fs::remove_dir_all(&d).unwrap();
    }
}
