//! Ancho en columnas de la terminal y recorte de texto a una celda.

/// Columnas que ocupa un caracter. Aproximación sin tablas Unicode completas:
/// marca como dobles los rangos CJK y de emoji más comunes, y como cero las
/// marcas combinantes y los selectores de variación.
pub fn char_width(c: char) -> usize {
    let cp = c as u32;
    match cp {
        0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f | 0x20d0..=0x20ff => 0,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// Reemplaza lo que rompería la pantalla: tabs a espacios, controles a '?'.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else if c.is_control() {
            out.push('?');
            col += 1;
        } else {
            out.push(c);
            col += char_width(c);
        }
    }
    out
}

/// Devuelve `s` ocupando exactamente `width` columnas: recortado con '…' si
/// no entra, o completado con espacios si sobra lugar.
pub fn fit(s: &str, width: usize) -> String {
    let total: usize = s.chars().map(char_width).sum();
    let mut out = String::with_capacity(width + 8);
    if total <= width {
        out.push_str(s);
        out.extend(std::iter::repeat_n(' ', width - total));
        return out;
    }
    if width == 0 {
        return out;
    }
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > width - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    used += 1;
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

/// Tamaño legible: 512B, 4.2K, 13M.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut v = bytes as f64;
    let mut unit = "B";
    for u in UNITS {
        v /= 1024.0;
        unit = u;
        if v < 1024.0 {
            break;
        }
    }
    if v < 10.0 {
        format!("{v:.1}{unit}")
    } else {
        format!("{v:.0}{unit}")
    }
}

/// Permisos al estilo `ls -l`: drwxr-xr-x.
pub fn mode_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o010000 => 'p',
        0o140000 => 's',
        0o020000 => 'c',
        0o060000 => 'b',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (bit, ch) in bits {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_pads_and_truncates() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abc", 3), "abc");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn fit_counts_wide_chars() {
        // Cada ideograma ocupa dos columnas.
        assert_eq!(fit("日本語", 6), "日本語");
        assert_eq!(fit("日本語", 5), "日本…");
        assert_eq!(fit("日本語x", 6), "日本… ");
    }

    #[test]
    fn sanitize_expands_tabs_and_controls() {
        assert_eq!(sanitize("a\tb"), "a   b");
        assert_eq!(sanitize("x\x1by"), "x?y");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(4300), "4.2K");
        assert_eq!(human_size(13 * 1024 * 1024), "13M");
    }

    #[test]
    fn modes() {
        assert_eq!(mode_string(0o040755), "drwxr-xr-x");
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
    }
}
