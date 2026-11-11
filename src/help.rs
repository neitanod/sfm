//! Pantalla de ayuda (tecla H).

use crate::text::fit;
use std::fmt::Write as _;

const STYLE_TITLE: &str = "\x1b[1;32m";
const STYLE_SECTION: &str = "\x1b[1;33m";
const STYLE_KEY: &str = "\x1b[1m";
const STYLE_DIM: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

/// Ancho máximo del texto de ayuda; en terminales anchas queda centrado.
const MAX_WIDTH: usize = 78;
/// Ancho de la columna de teclas.
const KEY_WIDTH: usize = 23;

enum Line {
    Title(&'static str),
    Section(&'static str),
    Key(&'static str, &'static str),
    Note(&'static str),
    Blank,
}

use Line::*;

const LINES: &[Line] = &[
    Title("sfm — Simple File Manager"),
    Blank,
    Section("START"),
    Key("sfm [DIR]", "open DIR, or the current folder"),
    Note("Left: folders of the parent folder. Center: the current folder, with the cursor. Right: a preview of the entry under the cursor."),
    Blank,
    Section("NAVIGATION"),
    Key("↑ ↓   k j", "move the cursor"),
    Key("←   h  Backspace", "go to the parent folder"),
    Key("→   l  Enter", "enter the folder under the cursor"),
    Key("PgUp  PgDn", "move one page"),
    Key("Home  End   g  G", "first / last entry"),
    Note("Each folder remembers where the cursor was when you left it."),
    Blank,
    Section("FILES"),
    Key("c", "mark the entry under the cursor; c on the marked entry unmarks it"),
    Key("p", "paste a copy of the marked entry here (cp -a, asks y/N)"),
    Key("m", "move the marked entry here (mv, asks y/N)"),
    Key("e", "edit the file with $VISUAL, $EDITOR, vim or vi"),
    Key("B", "back up to ~/sfm_backups/<name>_YYYY-MM-DD-HHMM (asks y/N)"),
    Key("D", "delete a file, symlink or empty folder (type Y to confirm)"),
    Note("The marked entry shows a yellow * in every column, and marking another entry replaces it."),
    Note("Paste and move never overwrite, refuse to put a folder inside itself, and clear the mark when done."),
    Note("B and D work only in uppercase, so a stray b or d does nothing."),
    Note("A second backup within the same minute gets a -2 suffix."),
    Note("D refuses folders that still have something inside."),
    Blank,
    Section("VIEW"),
    Key(".", "show / hide dotfiles"),
    Key("r   Ctrl-L", "reload"),
    Note("The bottom bar shows the position, permissions, size, symlink target, [hidden] when dotfiles are shown, and what is marked."),
    Note("A folder preview reads one screenful of entries, so huge folders open fast. A text file shows its first lines; binary and special files are never opened."),
    Note("Folders on screen refresh by themselves when they change. If the current folder is deleted or moved, sfm climbs to the nearest folder that still exists. Where inotify is unavailable, r reloads by hand."),
    Blank,
    Section("MOUSE"),
    Key("click", "move the cursor to that row"),
    Key("double-click", "enter the folder"),
    Key("click left column", "go to that folder"),
    Key("click preview", "enter the previewed folder with that entry selected"),
    Key("wheel", "move three rows"),
    Key("Shift + drag", "select text on screen"),
    Note("While a confirmation is on screen the mouse is ignored, so the wheel can't cancel it."),
    Blank,
    Section("OTHER"),
    Key("H   ?", "this help (↑ ↓ scroll it, any other key closes it)"),
    Key("q   Esc   Ctrl-C", "quit"),
];

/// Parte `text` en renglones de como mucho `width` columnas, por palabras.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    // Una palabra más larga que el renglón (una ruta, por ejemplo) se parte.
    let words = text.split_whitespace().flat_map(|w| {
        let chars: Vec<char> = w.chars().collect();
        chars
            .chunks(width)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
    });
    for word in words {
        let word = word.as_str();
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if candidate.chars().count() > width && !line.is_empty() {
            out.push(std::mem::take(&mut line));
            line = word.to_string();
        } else {
            line = candidate;
        }
    }
    out.push(line);
    out
}

/// La ayuda ya dibujada, un renglón por fila de pantalla, para `width` columnas.
fn rows_for(width: usize) -> Vec<String> {
    let key_w = KEY_WIDTH.min(width / 2);
    let mut out = Vec::new();
    for line in LINES {
        match line {
            Title(t) => out.push(format!("{STYLE_TITLE}{}{RESET}", fit(&format!(" {t}"), width))),
            Section(s) => out.push(format!("{STYLE_SECTION}{}{RESET}", fit(&format!(" {s}"), width))),
            Key(k, d) => {
                for (i, part) in wrap(d, width - key_w).into_iter().enumerate() {
                    let key = if i == 0 { format!("   {k}") } else { String::new() };
                    out.push(format!(
                        "{STYLE_KEY}{}{RESET}{}",
                        fit(&key, key_w),
                        fit(&part, width - key_w)
                    ));
                }
            }
            Note(n) => {
                for part in wrap(n, width.saturating_sub(3)) {
                    out.push(format!("{STYLE_DIM}{}{RESET}", fit(&format!("   {part}"), width)));
                }
            }
            Blank => out.push(fit("", width)),
        }
    }
    out
}

/// Scroll máximo para que la última línea quede en pantalla.
pub fn max_scroll(rows: usize, cols: usize) -> usize {
    rows_for(cols.min(MAX_WIDTH))
        .len()
        .saturating_sub(rows.saturating_sub(1))
}

/// Dibuja la ayuda a pantalla completa, desde la fila `scroll`.
pub fn render(rows: usize, cols: usize, scroll: usize) -> String {
    let width = cols.min(MAX_WIDTH);
    let margin = " ".repeat((cols - width) / 2);
    let right = " ".repeat(cols - width - margin.len());
    let body = rows.saturating_sub(1);
    let lines = rows_for(width);
    let blank = fit("", width);

    let mut out = String::with_capacity(rows * cols * 2);
    out.push_str("\x1b[H");
    for row in 0..body {
        let line = lines.get(scroll + row).unwrap_or(&blank);
        let _ = write!(out, "\x1b[{};1H{margin}{line}{right}", row + 1);
    }
    let footer = if scroll + body < lines.len() {
        " ↑↓ scroll · any other key closes the help"
    } else {
        " any key closes the help"
    };
    let _ = write!(
        out,
        "\x1b[{rows};1H{margin}{STYLE_DIM}{}{RESET}{right}",
        fit(footer, width)
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(s: &str) -> String {
        // Saca las secuencias de escape para mirar sólo el texto.
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn shows_every_section() {
        let text = visible(&render(200, 100, 0));
        for s in ["START", "NAVIGATION", "FILES", "VIEW", "MOUSE", "OTHER", "sfm_backups", "dotfiles", "Ctrl-L", "wheel", "-2 suffix"] {
            assert!(text.contains(s), "{s}");
        }
        assert!(text.contains("any key closes"));
    }

    #[test]
    fn scrolls_when_short() {
        let text = visible(&render(10, 80, 0));
        assert!(!text.contains("OTHER"));
        assert!(text.contains("scroll"));
        let text = visible(&render(10, 80, max_scroll(10, 80)));
        assert!(text.contains("OTHER"));
    }

    #[test]
    fn long_descriptions_wrap() {
        let text = visible(&render(80, 50, 0));
        assert!(!text.contains('…'), "{text}");
        assert!(text.contains("sfm_backups/"));
    }

    #[test]
    fn narrow_terminal_does_not_panic() {
        render(5, 10, 0);
        render(1, 1, 0);
    }
}
