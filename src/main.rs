//! sfm — file manager de terminal en tres columnas, sin dependencias.
//!
//! Izquierda: carpetas de la carpeta padre. Centro: la carpeta actual, con el
//! cursor. Derecha: preview de lo que está bajo el cursor.

mod clock;
mod help;
mod listing;
mod term;
mod text;
mod watch;

use listing::{Entry, Preview};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use term::{Key, MouseKind};
use text::fit;

const STYLE_DIR: &str = "\x1b[1;34m";
const STYLE_LINK: &str = "\x1b[36m";
const STYLE_SELECTED: &str = "\x1b[7m";
const STYLE_DIM: &str = "\x1b[90m";
const STYLE_HEADER: &str = "\x1b[1;32m";
const STYLE_ERROR: &str = "\x1b[31m";
const STYLE_PROMPT: &str = "\x1b[1;33m";
const STYLE_OK: &str = "\x1b[32m";
const STYLE_MARK: &str = "\x1b[1;33m";
const STYLE_DANGER: &str = "\x1b[1;97;41m";
const RESET: &str = "\x1b[0m";

const HELP: &str = "←↑↓→ move  c copy  p paste  m move  e edit  B backup  D delete  H help  q quit";
const HELP_SHORT: &str = "H help  q quit";
const BACKUP_DIR: &str = "sfm_backups";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Copy,
    Move,
    Delete,
    Backup,
}

impl Op {
    fn verb(self) -> &'static str {
        match self {
            Op::Copy => "copy",
            Op::Move => "move",
            Op::Delete => "delete",
            Op::Backup => "backup",
        }
    }
}

/// Operación esperando confirmación: y/N, o `Y` mayúscula para borrar.
/// En Delete, `dest` es igual a `src`.
#[derive(Debug)]
struct Pending {
    op: Op,
    src: PathBuf,
    dest: PathBuf,
}

/// Anchos de las columnas y alto de los paneles del último cuadro.
#[derive(Debug, Default, Clone, Copy)]
struct Layout {
    left: usize,
    center: usize,
    right: usize,
    height: usize,
}

/// Columna de la pantalla en la que cayó un click.
#[derive(Debug, PartialEq, Eq)]
enum Area {
    Left(usize),
    Center(usize),
    Right(usize),
    Outside,
}

/// Dos clicks en la misma fila dentro de este lapso son un doble click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Filas que mueve cada paso de la rueda.
const WHEEL_STEP: isize = 3;

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
    /// Dónde van los backups de B: ~/sfm_backups.
    backup_dir: Option<PathBuf>,
    preview_cache: Option<(PathBuf, usize, bool, Preview)>,
    /// Geometría del último cuadro dibujado, para ubicar los clicks.
    layout: Layout,
    /// Último click, para reconocer el doble click.
    last_click: Option<(Instant, usize, usize)>,
    /// Pantalla de ayuda abierta, con su línea de scroll.
    help: Option<usize>,
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
            backup_dir: std::env::var_os("HOME").map(|h| PathBuf::from(h).join(BACKUP_DIR)),
            preview_cache: None,
            layout: Layout::default(),
            last_click: None,
            help: None,
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

    /// Carpetas que hay que vigilar: la actual, la padre y la del preview.
    fn watch_paths(&self) -> (PathBuf, Option<PathBuf>, Option<PathBuf>) {
        let preview = self
            .selected()
            .filter(|e| e.is_dir)
            .and_then(|_| self.selected_path());
        (
            self.cwd.clone(),
            self.cwd.parent().map(Path::to_path_buf),
            preview,
        )
    }

    /// Aplica los cambios que avisó el disco. El cursor se queda sobre el
    /// mismo elemento; si ese elemento desapareció, sobre el que ocupó su lugar.
    fn apply(&mut self, c: watch::Changes) {
        if c.current_gone || !self.cwd.is_dir() {
            let mut dir = self.cwd.clone();
            while !dir.is_dir() && dir.pop() {}
            self.preview_cache = None;
            let gone = self.cwd.display().to_string();
            self.change_dir(dir, None);
            self.message = Some(Notice::Error(format!("{gone} was removed or moved")));
            return;
        }
        let selected = self.selected().map(|e| e.name.clone());
        if c.overflow || c.current {
            if let Ok(entries) = listing::read_dir(&self.cwd, self.show_hidden, false) {
                let idx = self.cursor;
                self.entries = entries;
                self.cursor = selected
                    .as_ref()
                    .and_then(|n| self.entries.iter().position(|e| &e.name == n))
                    .unwrap_or_else(|| idx.min(self.entries.len().saturating_sub(1)));
            }
        }
        if c.overflow || c.parent {
            self.load_parent();
        }
        let touched_selected = selected.is_some_and(|n| c.touched.contains(&n));
        if c.overflow || c.current || c.preview || touched_selected {
            self.preview_cache = None;
        }
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

    /// Marca lo que está bajo el cursor. Hay una sola marca: marcar otra cosa
    /// reemplaza la anterior, y marcar lo ya marcado lo desmarca.
    fn mark(&mut self) {
        let (Some(path), Some(e)) = (self.selected_path(), self.selected()) else {
            return;
        };
        let name = e.display.clone();
        if self.clipboard.as_ref() == Some(&path) {
            self.clipboard = None;
            self.message = Some(Notice::Info(format!("unmarked {name}")));
        } else {
            self.clipboard = Some(path);
            self.message = Some(Notice::Info(format!(
                "marked {name} — p pastes a copy here, m moves it here"
            )));
        }
    }

    /// Si la entrada `name` de la carpeta `dir` es la que está marcada.
    fn is_marked(&self, dir: Option<&Path>, e: &Entry) -> bool {
        match (&self.clipboard, dir) {
            (Some(c), Some(d)) => c.parent() == Some(d) && c.file_name() == Some(e.name.as_os_str()),
            _ => false,
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

    /// Pide confirmación dura para borrar lo que está bajo el cursor. Borra
    /// archivos, symlinks y carpetas vacías; una carpeta con contenido no.
    fn request_delete(&mut self) {
        let Some(path) = self.selected_path() else { return };
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                self.message = Some(Notice::Error(format!("{}: {e}", path.display())));
                return;
            }
        };
        if meta.is_dir() {
            let empty = std::fs::read_dir(&path)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false);
            if !empty {
                self.message = Some(Notice::Error(
                    "folder not empty: D only deletes files and empty folders".into(),
                ));
                return;
            }
        }
        self.pending = Some(Pending {
            op: Op::Delete,
            src: path.clone(),
            dest: path,
        });
    }

    /// Pide confirmación para copiar lo que está bajo el cursor a
    /// ~/sfm_backups/<nombre>_AAAA-MM-DD-HHMM.
    fn request_backup(&mut self) {
        let Some(src) = self.selected_path() else { return };
        let Some(dir) = self.backup_dir.clone() else {
            self.message = Some(Notice::Error("backup: $HOME is not set".into()));
            return;
        };
        let Some(name) = src.file_name() else { return };
        let dest = backup_dest(&dir, name, &clock::stamp());
        self.pending = Some(Pending {
            op: Op::Backup,
            src,
            dest,
        });
    }

    fn confirm(&mut self, p: Pending) {
        match p.op {
            Op::Copy | Op::Move => self.confirm_transfer(p),
            Op::Delete => self.confirm_delete(p),
            Op::Backup => self.confirm_backup(p),
        }
    }

    fn confirm_delete(&mut self, p: Pending) {
        let is_dir = std::fs::symlink_metadata(&p.src).is_ok_and(|m| m.is_dir());
        let result = if is_dir {
            std::fs::remove_dir(&p.src)
        } else {
            std::fs::remove_file(&p.src)
        };
        if let Err(e) = result {
            self.message = Some(Notice::Error(format!("delete failed: {e}")));
            return;
        }
        if self.clipboard.as_ref() == Some(&p.src) {
            self.clipboard = None;
        }
        // El cursor queda en la misma posición, sobre el elemento siguiente.
        let idx = self.cursor;
        self.preview_cache = None;
        self.change_dir(self.cwd.clone(), None);
        self.cursor = idx.min(self.entries.len().saturating_sub(1));
        self.message = Some(Notice::Info(format!("deleted {}", p.src.display())));
    }

    fn confirm_backup(&mut self, p: Pending) {
        if let Some(dir) = p.dest.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                self.message = Some(Notice::Error(format!(
                    "backup: can't create {}: {e}",
                    dir.display()
                )));
                return;
            }
        }
        match run_transfer(&p) {
            Ok(()) => {
                self.message = Some(Notice::Info(format!("backup: {}", p.dest.display())))
            }
            Err(e) => self.message = Some(Notice::Error(e)),
        }
    }

    fn confirm_transfer(&mut self, p: Pending) {
        let name = p.dest.file_name().map(|n| n.to_os_string());
        match run_transfer(&p) {
            Ok(()) => {
                // Hecha la operación, la marca ya cumplió.
                self.clipboard = None;
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

    /// En qué columna y en qué fila de los paneles (desde 0) cayó un click.
    fn area_at(&self, col: usize, row: usize) -> Area {
        let l = self.layout;
        if row < 2 || row > l.height + 1 || col == 0 {
            return Area::Outside;
        }
        let r = row - 2;
        let center_start = if l.left > 0 { l.left + 2 } else { 1 };
        let right_start = center_start + l.center + 1;
        if l.left > 0 && col <= l.left {
            Area::Left(r)
        } else if col >= center_start && col < center_start + l.center {
            Area::Center(r)
        } else if l.right > 0 && col >= right_start && col < right_start + l.right {
            Area::Right(r)
        } else {
            Area::Outside
        }
    }

    fn click_at(&mut self, col: usize, row: usize, now: Instant) {
        let double = self.last_click.is_some_and(|(t, c, r)| {
            now.duration_since(t) < DOUBLE_CLICK
                && r == row
                && self.area_at(c, r) == self.area_at(col, row)
        });
        // Tras un doble click, el tercero arranca de cero.
        self.last_click = if double { None } else { Some((now, col, row)) };
        match self.area_at(col, row) {
            Area::Center(r) => {
                let i = self.offset + r;
                if i < self.entries.len() {
                    self.cursor = i;
                    if double {
                        self.go_into();
                    }
                }
            }
            Area::Left(r) => {
                let (start, _) = self.parent_window(self.layout.height);
                let target = self.parent.get(start + r).map(|e| e.name.clone());
                if let (Some(name), Some(parent)) = (target, self.cwd.parent()) {
                    if Some(name.as_os_str()) != self.cwd.file_name() {
                        let dir = parent.join(name);
                        self.change_dir(dir, None);
                    }
                }
            }
            Area::Right(r) => {
                let name = match &self.preview_cache {
                    Some((_, _, _, Preview::Dir { entries, more })) => {
                        let shown = if *more && entries.len() >= self.layout.height {
                            self.layout.height.saturating_sub(1)
                        } else {
                            entries.len()
                        };
                        (r < shown).then(|| entries[r].name.clone())
                    }
                    _ => None,
                };
                if let (Some(name), Some(dir)) = (name, self.selected_path()) {
                    self.change_dir(dir, Some(name));
                }
            }
            Area::Outside => {}
        }
    }

    fn handle_mouse(&mut self, kind: MouseKind, col: usize, row: usize) {
        match kind {
            MouseKind::LeftPress => self.click_at(col, row, Instant::now()),
            MouseKind::WheelUp => self.move_cursor(-WHEEL_STEP),
            MouseKind::WheelDown => self.move_cursor(WHEEL_STEP),
            MouseKind::Other => {}
        }
    }

    /// Con la ayuda abierta, las flechas, las páginas y la rueda la desplazan
    /// y cualquier otra tecla la cierra.
    fn handle_help(&mut self, key: Key, scroll: usize) {
        let rows = self.layout.height + 2;
        let page = rows.saturating_sub(2).max(1) as isize;
        let delta = match key {
            Key::Up | Key::Char('k') => -1,
            Key::Down | Key::Char('j') => 1,
            Key::PageUp => -page,
            Key::PageDown => page,
            Key::Mouse { kind: MouseKind::WheelUp, .. } => -WHEEL_STEP,
            Key::Mouse { kind: MouseKind::WheelDown, .. } => WHEEL_STEP,
            Key::Mouse { kind: MouseKind::Other, .. } => 0,
            _ => {
                self.help = None;
                return;
            }
        };
        let cols = self.layout.left + self.layout.center + self.layout.right + if self.layout.left > 0 { 2 } else { 0 };
        let max = help::max_scroll(rows, cols) as isize;
        self.help = Some((scroll as isize + delta).clamp(0, max) as usize);
    }

    fn handle(&mut self, key: Key, page: usize) {
        if let Some(scroll) = self.help {
            self.handle_help(key, scroll);
            return;
        }
        if let Key::Mouse { kind, col, row } = key {
            // Con una confirmación en pantalla, el mouse no hace nada: así
            // un movimiento de la rueda no la cancela.
            if self.pending.is_none() && kind != MouseKind::Other {
                self.message = None;
                self.handle_mouse(kind, col, row);
            }
            return;
        }
        self.message = None;
        if let Some(p) = self.pending.take() {
            // Borrar exige la Y mayúscula; lo demás acepta y o Y.
            let confirmed = match p.op {
                Op::Delete => key == Key::Char('Y'),
                _ => matches!(key, Key::Char('y') | Key::Char('Y')),
            };
            if confirmed {
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
            Key::Char('D') => self.request_delete(),
            Key::Char('B') => self.request_backup(),
            Key::Char('H') | Key::Char('?') => self.help = Some(0),
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
        self.layout = Layout {
            left: left_w,
            center: center_w,
            right: right_w,
            height,
        };
        if let Some(scroll) = self.help {
            return help::render(rows, cols, scroll.min(help::max_scroll(rows, cols)));
        }

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

    fn entry_cell(e: &Entry, width: usize, selected: bool, marked: bool) -> String {
        if width == 0 {
            return String::new();
        }
        let mut name = e.display.clone();
        if e.is_dir {
            name.push('/');
        }
        let style = if e.broken {
            STYLE_ERROR
        } else if e.is_link {
            STYLE_LINK
        } else if e.is_dir {
            STYLE_DIR
        } else {
            ""
        };
        let sel = if selected { STYLE_SELECTED } else { "" };
        // La primera columna de la celda lleva el indicador de marca.
        let mark = if marked {
            format!("{STYLE_MARK}{sel}*{RESET}")
        } else {
            format!("{sel} {RESET}")
        };
        let avail = width - 1;
        let name_w: usize = name.chars().map(text::char_width).sum();
        // El destino del symlink va en gris después del nombre; si no entra
        // todo, se recorta primero el destino.
        match &e.link_target {
            Some(target) if name_w + 4 < avail => format!(
                "{mark}{style}{sel}{name}{RESET}{STYLE_DIM}{sel}{}{RESET}",
                fit(&format!(" -> {target}"), avail - name_w)
            ),
            _ => format!("{mark}{style}{sel}{}{RESET}", fit(&name, avail)),
        }
    }

    /// Primera fila visible de la columna izquierda y posición de la carpeta
    /// actual en ella. La carpeta actual queda centrada.
    fn parent_window(&self, height: usize) -> (usize, Option<usize>) {
        let current = self.cwd.file_name();
        let idx = self
            .parent
            .iter()
            .position(|e| Some(e.name.as_os_str()) == current);
        let start = idx
            .unwrap_or(0)
            .saturating_sub(height / 2)
            .min(self.parent.len().saturating_sub(height));
        (start, idx)
    }

    fn left_column(&self, height: usize, width: usize) -> Vec<String> {
        let parent_dir = self.cwd.parent();
        let (start, idx) = self.parent_window(height);
        (0..height)
            .map(|row| match self.parent.get(start + row) {
                Some(e) => Self::entry_cell(
                    e,
                    width,
                    Some(start + row) == idx,
                    self.is_marked(parent_dir, e),
                ),
                None => fit("", width),
            })
            .collect()
    }

    fn center_column(&self, height: usize, width: usize) -> Vec<String> {
        (0..height)
            .map(|row| {
                let i = self.offset + row;
                match self.entries.get(i) {
                    Some(e) => Self::entry_cell(
                        e,
                        width,
                        i == self.cursor,
                        self.is_marked(Some(&self.cwd), e),
                    ),
                    None if i == 0 => format!("{STYLE_DIM}{}{RESET}", fit("  (empty)", width)),
                    None => fit("", width),
                }
            })
            .collect()
    }

    fn right_column(&mut self, height: usize, width: usize) -> Vec<String> {
        // Lo marcado, visto desde la carpeta que muestra el preview.
        let shown = self.selected_path();
        let marked_name = match (&self.clipboard, &shown) {
            (Some(c), Some(dir)) if c.parent() == Some(dir.as_path()) => {
                c.file_name().map(|n| n.to_os_string())
            }
            _ => None,
        };
        let mut lines: Vec<String> = match self.preview(height) {
            None => Vec::new(),
            Some(Preview::Dir { entries, more }) => {
                let mut v: Vec<String> = entries
                    .iter()
                    .take(height)
                    .map(|e| Self::entry_cell(e, width, false, marked_name.as_ref() == Some(&e.name)))
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
            let (style, q) = match p.op {
                Op::Copy | Op::Move => (
                    STYLE_PROMPT,
                    format!(
                        " {} {} → {}/ ? [y/N]",
                        p.op.verb(),
                        p.src.display(),
                        self.cwd.display()
                    ),
                ),
                Op::Backup => (
                    STYLE_PROMPT,
                    format!(" backup {} → {} ? [y/N]", p.src.display(), p.dest.display()),
                ),
                Op::Delete => (
                    STYLE_DANGER,
                    format!(
                        " DELETE {} ? Type Y (uppercase) to confirm, any other key cancels",
                        p.src.display()
                    ),
                ),
            };
            return format!("{style}{}{RESET}", fit(&text::sanitize(&q), cols));
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

/// Destino de un backup: <dir>/<nombre>_<stamp>, o con -2, -3… si en el
/// mismo minuto ya se hizo otro.
fn backup_dest(dir: &Path, name: &std::ffi::OsStr, stamp: &str) -> PathBuf {
    let mut base = name.to_os_string();
    base.push("_");
    base.push(stamp);
    let first = dir.join(&base);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    (2..)
        .map(|n| {
            let mut alt = base.clone();
            alt.push(format!("-{n}"));
            dir.join(alt)
        })
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("some free name")
}

/// Corre `cp -a` (copia y backup) o `mv` y devuelve el error de la
/// herramienta si falla.
fn run_transfer(p: &Pending) -> Result<(), String> {
    let mut cmd = match p.op {
        Op::Delete => return Err("delete is not a transfer".into()),
        Op::Copy | Op::Backup => {
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
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg("sfm")
        .arg(path);
    if let Some(tty) = term::tty_stdout() {
        cmd.stdout(tty);
    }
    cmd.status().map(|_| ())
}

const USAGE: &str = "usage: sfm [-d] [DIR]

  DIR   folder to open (default: the current folder)
  -d    on quit, print `cd <folder>` with the folder you were in.
        With a shell function the shell follows you:
          s() { eval \"$(sfm -d \"$@\")\"; }

Press H inside sfm for every key.";

#[derive(Debug, Default, PartialEq)]
struct Args {
    dir: Option<std::ffi::OsString>,
    print_cd: bool,
    help: bool,
}

fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut options = true;
    for a in args {
        match a.to_str() {
            Some("--") if options => options = false,
            Some("-d") if options => out.print_cd = true,
            Some("-h" | "--help") if options => out.help = true,
            Some(o) if options && o.starts_with('-') && o.len() > 1 => {
                return Err(format!("unknown option {o}\n\n{USAGE}"));
            }
            _ if out.dir.is_some() => return Err(format!("only one folder, please\n\n{USAGE}")),
            _ => out.dir = Some(a),
        }
    }
    Ok(out)
}

/// Una ruta lista para pegar en la shell: tal cual si sólo tiene caracteres
/// seguros, y si no entre comillas simples.
fn shell_quote(path: &Path) -> Vec<u8> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let safe = |b: &u8| b.is_ascii_alphanumeric() || b"/._-+,:@%".contains(b);
    if !bytes.is_empty() && bytes.iter().all(safe) {
        return bytes.to_vec();
    }
    let mut out = vec![b'\''];
    for &b in bytes {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

fn start_dir(arg: Option<std::ffi::OsString>) -> Result<PathBuf, String> {
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

/// Cada cuánto se aplican, como mucho, los avisos del disco. Una copia de mil
/// archivos genera mil avisos; así la pantalla se rehace diez veces por
/// segundo y no mil.
const REFRESH_EVERY: Duration = Duration::from_millis(100);
/// Cada cuánto se revisa el tamaño de la ventana si no pasa nada.
const IDLE_WAIT_MS: i32 = 250;

/// Corre la interfaz y devuelve la carpeta en la que estaba al salir.
fn run(start: &Path) -> std::io::Result<PathBuf> {
    let mut app = App::new(start.to_path_buf())?;
    let mut watcher = watch::Watcher::new();
    let mut changes = watch::Changes::default();
    let mut last_apply = Instant::now() - REFRESH_EVERY;
    let mut out = term::writer();
    let mut last_size = (0, 0);
    let mut dirty = true;
    while !app.quit {
        if let Some(w) = watcher.as_mut() {
            let (cur, parent, preview) = app.watch_paths();
            w.sync(&cur, parent.as_deref(), preview.as_deref());
        }
        let (rows, cols) = term::size();
        if dirty || (rows, cols) != last_size {
            out.write_all(app.render(rows, cols).as_bytes())?;
            out.flush()?;
            last_size = (rows, cols);
            dirty = false;
        }

        let timeout = if changes.is_empty() {
            IDLE_WAIT_MS
        } else {
            REFRESH_EVERY.saturating_sub(last_apply.elapsed()).as_millis() as i32
        };
        let (keys_ready, fs_ready) = term::wait(watcher.as_ref().map(|w| w.fd()), timeout);
        if fs_ready {
            if let Some(w) = watcher.as_mut() {
                changes.merge(w.drain());
            }
        }
        if !changes.is_empty() && last_apply.elapsed() >= REFRESH_EVERY {
            app.apply(std::mem::take(&mut changes));
            last_apply = Instant::now();
            dirty = true;
        }
        if !keys_ready {
            continue;
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
    Ok(app.cwd)
}

fn main() {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("sfm: {e}");
            std::process::exit(2);
        }
    };
    if args.help {
        println!("{USAGE}");
        return;
    }
    let start = match start_dir(args.dir) {
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
    match result {
        Ok(cwd) if args.print_cd => {
            let mut line = b"cd ".to_vec();
            line.extend(shell_quote(&cwd));
            line.push(b'\n');
            let _ = std::io::stdout().write_all(&line);
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("sfm: {e}");
            std::process::exit(1);
        }
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
        // Pegar desmarca
        assert!(app.clipboard.is_none());

        // Pegar otra vez en el mismo lugar no pisa
        app.handle(Key::Left, 10);
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('c'), 10);
        select(&mut app, "beta");
        app.handle(Key::Right, 10);
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

    #[test]
    fn marking_replaces_and_toggles() {
        let d = tree("marks");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('c'), 10);
        select(&mut app, "beta");
        app.handle(Key::Char('c'), 10);
        assert_eq!(app.clipboard, Some(d.join("beta")));
        // La marca se ve en la lista y la anterior ya no
        let frame = app.render(10, 100);
        assert!(frame.contains("*\x1b[0m\x1b[1;34m\x1b[7mbeta/"));
        assert_eq!(frame.matches("\x1b[1;33m").count(), 1);
        app.handle(Key::Char('c'), 10);
        assert!(app.clipboard.is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn delete_needs_uppercase_y() {
        let d = tree("delete");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "zfile.txt");
        let idx = app.cursor;
        app.handle(Key::Char('d'), 10); // minúscula: nada
        assert!(app.pending.is_none());
        app.handle(Key::Char('D'), 10);
        app.handle(Key::Char('y'), 10); // minúscula cancela
        assert!(d.join("zfile.txt").exists());
        app.handle(Key::Char('D'), 10);
        app.handle(Key::Enter, 10); // Enter también cancela
        assert!(d.join("zfile.txt").exists());
        app.handle(Key::Char('c'), 10);
        app.handle(Key::Char('D'), 10);
        app.handle(Key::Char('Y'), 10);
        assert!(!d.join("zfile.txt").exists());
        assert!(app.clipboard.is_none()); // estaba marcado: se desmarca
        assert_eq!(app.cursor, idx.min(app.entries.len() - 1));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn delete_only_empty_folders() {
        let d = tree("delete-dir");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "alpha");
        app.handle(Key::Char('D'), 10);
        assert!(app.pending.is_none());
        assert!(matches!(app.message, Some(Notice::Error(_))));
        select(&mut app, "beta");
        app.handle(Key::Char('D'), 10);
        app.handle(Key::Char('Y'), 10);
        assert!(!d.join("beta").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn backup_with_timestamp_and_no_overwrite() {
        let d = tree("backup");
        let mut app = App::new(d.clone()).unwrap();
        let bdir = d.join("home/sfm_backups");
        app.backup_dir = Some(bdir.clone());
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('b'), 10); // minúscula: nada
        assert!(app.pending.is_none());
        app.handle(Key::Char('B'), 10);
        app.handle(Key::Char('n'), 10);
        assert!(!bdir.exists());
        app.handle(Key::Char('B'), 10);
        app.handle(Key::Char('y'), 10);
        app.handle(Key::Char('B'), 10);
        app.handle(Key::Char('y'), 10);
        let mut names: Vec<String> = fs::read_dir(&bdir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].starts_with("zfile.txt_20"), "{names:?}");
        assert_eq!(names[0].len(), "zfile.txt_2026-10-25-1235".len());
        // Si cambió el minuto entre los dos, igual son dos archivos distintos.
        assert!(names[1].ends_with("-2") || names[1] != names[0]);
        assert_eq!(fs::read_to_string(bdir.join(&names[0])).unwrap(), "hello\n");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn backup_reports_when_dir_cannot_be_created() {
        let d = tree("backup-fail");
        let mut app = App::new(d.clone()).unwrap();
        // Un archivo donde debería ir la carpeta: create_dir_all falla.
        fs::write(d.join("blocker"), "").unwrap();
        app.backup_dir = Some(d.join("blocker/sfm_backups"));
        app.reload();
        select(&mut app, "zfile.txt");
        app.handle(Key::Char('B'), 10);
        app.handle(Key::Char('y'), 10);
        match &app.message {
            Some(Notice::Error(m)) => assert!(m.contains("can't create"), "{m}"),
            _ => panic!("expected an error"),
        }
        fs::remove_dir_all(&d).unwrap();
    }

    fn changes() -> watch::Changes {
        watch::Changes::default()
    }

    #[test]
    fn live_refresh_keeps_selection() {
        let d = tree("live");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "beta");
        fs::write(d.join("aaa.txt"), "").unwrap();
        fs::create_dir(d.join("aaa")).unwrap();
        app.apply(watch::Changes { current: true, ..changes() });
        assert_eq!(app.selected().unwrap().display, "beta");
        assert!(app.entries.iter().any(|e| e.display == "aaa.txt"));

        // Si lo seleccionado desaparece, el cursor queda en la misma posición
        let idx = app.cursor;
        fs::remove_dir(d.join("beta")).unwrap();
        app.apply(watch::Changes { current: true, ..changes() });
        assert_eq!(app.cursor, idx);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn live_refresh_climbs_when_cwd_is_removed() {
        let d = tree("climb");
        let mut app = App::new(d.join("alpha/inner")).unwrap();
        fs::remove_dir_all(d.join("alpha")).unwrap();
        app.apply(watch::Changes { current_gone: true, ..changes() });
        assert_eq!(app.cwd, d);
        assert!(matches!(app.message, Some(Notice::Error(_))));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn touched_selected_file_refreshes_preview() {
        let d = tree("touch");
        let mut app = App::new(d.clone()).unwrap();
        select(&mut app, "zfile.txt");
        assert!(app.render(10, 100).contains("hello"));
        fs::write(d.join("zfile.txt"), "bye\n").unwrap();
        app.apply(watch::Changes { touched: vec!["zfile.txt".into()], ..changes() });
        assert!(app.render(10, 100).contains("bye"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn watch_paths_follow_selection() {
        let d = tree("paths");
        let mut app = App::new(d.join("alpha")).unwrap();
        select(&mut app, "inner");
        let (cur, parent, preview) = app.watch_paths();
        assert_eq!(cur, d.join("alpha"));
        assert_eq!(parent, Some(d.clone()));
        assert_eq!(preview, Some(d.join("alpha/inner")));
        select(&mut app, "notes.txt");
        assert_eq!(app.watch_paths().2, None);
        fs::remove_dir_all(&d).unwrap();
    }

    /// Dibuja un cuadro de 100x12: izquierda 18 columnas, centro de la 20 a
    /// la 63, derecha desde la 65. Los paneles van de la fila 2 a la 11.
    fn drawn(dir: PathBuf) -> App {
        let mut app = App::new(dir).unwrap();
        app.render(12, 100);
        app
    }

    #[test]
    fn click_moves_cursor_and_double_click_enters() {
        let d = tree("click");
        let mut app = drawn(d.clone());
        let t0 = Instant::now();
        // Fila 3 de la pantalla = segunda entrada (beta)
        app.click_at(30, 3, t0);
        assert_eq!(app.selected().unwrap().display, "beta");
        assert_eq!(app.cwd, d);
        app.click_at(30, 2, t0 + Duration::from_millis(500)); // lento: no es doble
        assert_eq!(app.cwd, d);
        app.click_at(30, 2, t0 + Duration::from_millis(600));
        assert_eq!(app.cwd, d.join("alpha"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn double_click_on_file_only_selects() {
        let d = tree("dblfile");
        let mut app = drawn(d.clone());
        let t0 = Instant::now();
        app.click_at(30, 4, t0);
        app.click_at(30, 4, t0 + Duration::from_millis(100));
        assert_eq!(app.cwd, d);
        assert_eq!(app.selected().unwrap().display, "zfile.txt");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn click_on_left_column_goes_to_that_folder() {
        let d = tree("leftclick");
        let mut app = drawn(d.join("alpha"));
        // Columna izquierda: alpha/ en la fila 2, beta/ en la 3
        app.click_at(5, 3, Instant::now());
        assert_eq!(app.cwd, d.join("beta"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn click_on_preview_enters_with_that_entry() {
        let d = tree("rightclick");
        let mut app = drawn(d.clone()); // alpha seleccionada; preview: inner/, notes.txt
        app.click_at(70, 3, Instant::now());
        assert_eq!(app.cwd, d.join("alpha"));
        assert_eq!(app.selected().unwrap().display, "notes.txt");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn wheel_moves_and_mouse_keeps_confirmation() {
        let d = tree("wheel");
        let mut app = drawn(d.clone());
        app.handle(Key::Mouse { kind: MouseKind::WheelDown, col: 30, row: 3 }, 10);
        assert_eq!(app.cursor, 2);
        app.handle(Key::Char('D'), 10);
        app.handle(Key::Mouse { kind: MouseKind::WheelUp, col: 30, row: 3 }, 10);
        assert!(app.pending.is_some());
        assert_eq!(app.cursor, 2);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn help_opens_scrolls_and_closes_without_quitting() {
        let d = tree("help");
        let mut app = drawn(d.clone()); // 12 filas
        app.handle(Key::Char('h'), 10); // minúscula: sube de carpeta, no es la ayuda
        assert!(app.help.is_none());
        app.handle(Key::Char('l'), 10);
        app.handle(Key::Char('H'), 10);
        assert!(app.render(12, 100).contains("NAVIGATION"));
        app.handle(Key::Down, 10);
        assert_eq!(app.help, Some(1));
        let cursor = app.cursor;
        app.handle(Key::Char('q'), 10);
        assert!(app.help.is_none());
        assert!(!app.quit);
        assert_eq!(app.cursor, cursor); // la flecha movió la ayuda, no el cursor
        app.handle(Key::Char('?'), 10);
        assert!(app.help.is_some());
        app.handle(Key::Mouse { kind: MouseKind::LeftPress, col: 1, row: 1 }, 10);
        assert!(app.help.is_none());
        fs::remove_dir_all(&d).unwrap();
    }

    fn os(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(|s| s.into()).collect()
    }

    #[test]
    fn args_parsing() {
        assert_eq!(parse_args(os(&[])).unwrap(), Args::default());
        let a = parse_args(os(&["-d", "/tmp"])).unwrap();
        assert!(a.print_cd);
        assert_eq!(a.dir, Some("/tmp".into()));
        let a = parse_args(os(&["/tmp", "-d"])).unwrap();
        assert!(a.print_cd);
        assert!(parse_args(os(&["-x"])).is_err());
        assert!(parse_args(os(&["a", "b"])).is_err());
        // Después de -- un nombre con guión es una carpeta
        assert_eq!(parse_args(os(&["--", "-d"])).unwrap().dir, Some("-d".into()));
        assert!(parse_args(os(&["--help"])).unwrap().help);
    }

    #[test]
    fn shell_quoting() {
        let q = |p: &str| String::from_utf8(shell_quote(Path::new(p))).unwrap();
        assert_eq!(q("/home/sebas/doc"), "/home/sebas/doc");
        assert_eq!(q("/tmp/con espacio"), "'/tmp/con espacio'");
        assert_eq!(q("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(q("/tmp/$HOME"), "'/tmp/$HOME'");
        assert_eq!(q("/tmp/ñandú"), "'/tmp/ñandú'");
    }

    #[test]
    fn symlinks_show_their_target_on_the_same_row() {
        let d = tree("linkrow");
        std::os::unix::fs::symlink("alpha/notes.txt", d.join("to-notes")).unwrap();
        std::os::unix::fs::symlink("gone", d.join("broken")).unwrap();
        let mut app = drawn(d.clone());
        let plain = |s: String| {
            let mut out = String::new();
            let mut it = s.chars();
            while let Some(c) = it.next() {
                if c == '\x1b' {
                    for c2 in it.by_ref() {
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        };
        let frame = plain(app.render(12, 120));
        assert!(frame.contains("to-notes -> alpha/notes.txt"), "{frame}");
        assert!(frame.contains("broken -> gone"));
        // Rojo para el roto
        assert!(app.render(12, 120).contains("\x1b[31mbroken"));
        // Angosto: el nombre se ve aunque el destino no entre
        let narrow = plain(app.render(12, 40));
        assert!(narrow.contains("to-not"), "{narrow}");
        fs::remove_dir_all(&d).unwrap();
    }
}
