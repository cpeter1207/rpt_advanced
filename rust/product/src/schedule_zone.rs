//! Capture the host IANA zone once during generation preparation.
use std::{
    ffi::OsStr,
    path::{Component, Path},
};

/// Resolve TZ or the system localtime file. None leaves zone-less expressions unavailable.
pub fn capture() -> Option<String> {
    resolve(
        std::env::var_os("TZ").as_deref(),
        Path::new("/usr/share/zoneinfo"),
        Path::new("/etc/localtime"),
    )
}
fn resolve(setting: Option<&OsStr>, zoneinfo: &Path, localtime: &Path) -> Option<String> {
    let root = zoneinfo.canonicalize().ok()?;
    let selected = match setting {
        Some(s) => {
            let s = s.to_str()?;
            let s = s.strip_prefix(':').unwrap_or(s);
            if s.is_empty() {
                return None;
            }
            let p = Path::new(s);
            if p.is_relative() && !p.components().all(|c| matches!(c, Component::Normal(_))) {
                return None;
            }
            root.join(p)
        }
        None => localtime.to_owned(),
    };
    let path = selected.canonicalize().ok()?;
    let zone = path.strip_prefix(root).ok()?.to_str()?;
    (!zone.is_empty() && path.is_file()).then(|| zone.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_explicit_iana_zone_or_the_actual_localtime_symlink() {
        let db = Path::new("/usr/share/zoneinfo");
        assert_eq!(
            resolve(
                Some(OsStr::new("America/Chicago")),
                db,
                Path::new("/unused")
            ),
            Some("America/Chicago".into())
        );
        assert_eq!(
            resolve(
                Some(OsStr::new(":/usr/share/zoneinfo/Australia/Lord_Howe")),
                db,
                Path::new("/unused")
            )
            .as_deref(),
            Some("Australia/Lord_Howe")
        );
        assert!(
            resolve(
                Some(OsStr::new(":relative/../outside")),
                db,
                db.join("Etc/UTC").as_path()
            )
            .is_none()
        );
        assert_eq!(
            resolve(None, db, db.join("Pacific/Apia").as_path()).as_deref(),
            Some("Pacific/Apia")
        );
        assert!(resolve(None, db, Path::new("/nonexistent")).is_none());
    }
}
