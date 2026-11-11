//! Fecha y hora local sin crates: `time` y `localtime_r` de la libc.
//!
//! `localtime_r` toma la zona horaria de TZ o de /etc/localtime, tanto en
//! glibc como en musl.

#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

unsafe extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
}

/// Marca de tiempo local para nombres de backup: 2026-10-25-1235.
pub fn stamp() -> String {
    let now = unsafe { time(std::ptr::null_mut()) };
    let mut tm = unsafe { std::mem::zeroed::<Tm>() };
    if unsafe { localtime_r(&now, &mut tm) }.is_null() {
        return format!("t{now}");
    }
    format_stamp(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
    )
}

pub fn format_stamp(year: i32, month: i32, day: i32, hour: i32, min: i32) -> String {
    format!("{year:04}-{month:02}-{day:02}-{hour:02}{min:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_format() {
        assert_eq!(format_stamp(2026, 10, 25, 12, 35), "2026-10-25-1235");
        assert_eq!(format_stamp(2026, 1, 5, 9, 7), "2026-01-05-0907");
    }

    #[test]
    fn stamp_matches_date_command() {
        let out = std::process::Command::new("date")
            .arg("+%Y-%m-%d-%H%M")
            .output();
        if let Ok(out) = out {
            let date = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let mine = stamp();
            // Puede cambiar el minuto entre las dos lecturas.
            assert_eq!(mine[..13], date[..13], "{mine} vs {date}");
        }
    }
}
