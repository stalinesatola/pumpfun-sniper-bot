//! Small JSON files the bot keeps between runs (settings changed from the dashboard).
use std::io;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Writes `value` as pretty JSON, atomically: a crash mid-write never leaves a half-written
/// file behind (the data goes to a temporary file that is then renamed over the target).
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

/// What reading a saved file gave.
#[derive(Debug, PartialEq)]
pub enum Loaded<T> {
    /// No file yet (first run).
    Missing,
    Value(T),
    /// The file exists but could not be read or parsed; the message says why.
    Unreadable(String),
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Loaded<T> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(v) => Loaded::Value(v),
            Err(e) => Loaded::Unreadable(e.to_string()),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::Missing,
        Err(e) => Loaded::Unreadable(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct Sample {
        a: u32,
        b: String,
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sniper-persist-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip_missing_and_corrupt() {
        let d = dir("rt");
        let path = d.join("x.json");
        assert_eq!(read_json::<Sample>(&path), Loaded::Missing);

        let v = Sample {
            a: 7,
            b: "hi".into(),
        };
        write_json(&path, &v).unwrap();
        assert!(
            !path.with_extension("json.tmp").exists(),
            "no temp file is left"
        );
        assert_eq!(read_json::<Sample>(&path), Loaded::Value(v));

        std::fs::write(&path, "{ nope").unwrap();
        assert!(matches!(read_json::<Sample>(&path), Loaded::Unreadable(_)));
        std::fs::write(&path, r#"{"a":"not a number","b":"x"}"#).unwrap();
        assert!(matches!(read_json::<Sample>(&path), Loaded::Unreadable(_)));

        assert!(write_json(
            &d.join("missing-dir/x.json"),
            &Sample {
                a: 1,
                b: String::new()
            }
        )
        .is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
