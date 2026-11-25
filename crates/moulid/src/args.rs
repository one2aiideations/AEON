//! Command-line options.

use std::path::PathBuf;

/// Default inference server binary.
pub const DEFAULT_SERVER: &str = "/usr/bin/llama-server";
/// Default model directory.
pub const DEFAULT_MODEL_DIR: &str = "/mouli/models";

/// Parsed options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Inference server binary.
    pub server: PathBuf,
    /// Explicit model path.
    pub model: Option<PathBuf>,
    /// Server port on 127.0.0.1.
    pub port: u16,
    /// Run one prompt and power off.
    pub selftest: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
            model: None,
            port: 8080,
            selftest: false,
        }
    }
}

impl Args {
    /// Parse `--server PATH --model PATH --port N --selftest`.
    pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut a = Self::default();
        let mut it = args.into_iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
            match flag.as_str() {
                "--server" => a.server = value()?.into(),
                "--model" => a.model = Some(value()?.into()),
                "--port" => {
                    a.port = value()?
                        .parse()
                        .map_err(|_| "--port needs a number 1-65535".to_string())?;
                    if a.port == 0 {
                        return Err("--port needs a number 1-65535".into());
                    }
                }
                "--selftest" => a.selftest = true,
                other => return Err(format!("unknown option {other}")),
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(v: &[&str]) -> Result<Args, String> {
        Args::parse(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_flags() {
        let a = p(&["--port", "9000", "--selftest", "--model", "/m.gguf"]).unwrap();
        assert_eq!(a.port, 9000);
        assert!(a.selftest);
        assert_eq!(a.model.unwrap(), PathBuf::from("/m.gguf"));
        assert_eq!(p(&[]).unwrap(), Args::default());
    }

    #[test]
    fn rejects_bad() {
        assert!(p(&["--port"]).is_err());
        assert!(p(&["--port", "0"]).is_err());
        assert!(p(&["--port", "x"]).is_err());
        assert!(p(&["--bogus"]).is_err());
    }
}
