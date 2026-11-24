//! Stopping the text console from drawing over the screen, and putting it back afterwards.
//!
//! The kernel's framebuffer console (`fbcon`) repaints the screen as text whenever something is
//! printed. While the graphical screens run, it is unbound from the virtual terminal through
//! sysfs (`/sys/class/vtconsole/vtconN/bind`) and its blinking cursor is turned off; both are
//! restored when the guard is dropped. Nothing here needs `unsafe` or an ioctl.

use std::fs;
use std::path::{Path, PathBuf};

/// Holds the console away from the screen until dropped.
#[derive(Debug)]
pub struct ConsoleTakeover {
    sys: PathBuf,
    restore: Vec<(PathBuf, String)>,
}

impl ConsoleTakeover {
    /// A takeover for the sysfs tree rooted at `sys` (normally `/sys`). Nothing changes until
    /// [`ConsoleTakeover::engage`].
    pub fn new(sys: &Path) -> Self {
        Self {
            sys: sys.to_path_buf(),
            restore: Vec::new(),
        }
    }

    /// Turns off the console cursor blink and unbinds the framebuffer console. Returns notes about
    /// anything that could not be changed (the screens still work; the text console may flicker).
    pub fn engage(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let change = |this: &mut Self, path: PathBuf, value: &str, notes: &mut Vec<String>| {
            match fs::read_to_string(&path) {
                Ok(old) => {
                    let old = old.trim().to_string();
                    if old == value {
                        return;
                    }
                    match fs::write(&path, value) {
                        Ok(()) => this.restore.push((path, old)),
                        Err(e) => notes.push(format!("{}: {e}", path.display())),
                    }
                }
                Err(e) => notes.push(format!("{}: {e}", path.display())),
            }
        };
        change(
            self,
            self.sys.join("class/graphics/fbcon/cursor_blink"),
            "0",
            &mut notes,
        );
        let vt_dir = self.sys.join("class/vtconsole");
        if let Ok(entries) = fs::read_dir(&vt_dir) {
            let mut dirs: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
            dirs.sort();
            for d in dirs {
                let is_fbcon =
                    fs::read_to_string(d.join("name")).is_ok_and(|n| n.contains("frame buffer"));
                if is_fbcon {
                    change(self, d.join("bind"), "0", &mut notes);
                }
            }
        } else {
            notes.push(format!("{}: not found", vt_dir.display()));
        }
        notes
    }

    /// Puts everything back as it was (also done on drop).
    pub fn release(&mut self) {
        while let Some((path, old)) = self.restore.pop() {
            let _ = fs::write(path, old);
        }
    }
}

impl Drop for ConsoleTakeover {
    fn drop(&mut self) {
        self.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|t| t.as_nanos())
            .unwrap_or(0);
        let d =
            std::env::temp_dir().join(format!("mouli-con-{tag}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn get(root: &Path, rel: &str) -> String {
        fs::read_to_string(root.join(rel))
            .unwrap()
            .trim()
            .to_string()
    }

    /// A sysfs tree like a real machine's: vtcon0 is the dummy console, vtcon1 the framebuffer one.
    fn sysfs() -> PathBuf {
        let root = tmp("tree");
        put(&root, "class/graphics/fbcon/cursor_blink", "1\n");
        put(&root, "class/vtconsole/vtcon0/name", "(S) dummy device\n");
        put(&root, "class/vtconsole/vtcon0/bind", "0\n");
        put(
            &root,
            "class/vtconsole/vtcon1/name",
            "(M) frame buffer device\n",
        );
        put(&root, "class/vtconsole/vtcon1/bind", "1\n");
        root
    }

    #[test]
    fn engage_unbinds_only_the_framebuffer_console_and_release_restores() {
        let root = sysfs();
        let mut c = ConsoleTakeover::new(&root);
        let notes = c.engage();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(get(&root, "class/vtconsole/vtcon1/bind"), "0");
        assert_eq!(get(&root, "class/graphics/fbcon/cursor_blink"), "0");
        assert_eq!(
            get(&root, "class/vtconsole/vtcon0/bind"),
            "0",
            "the dummy console is left alone"
        );
        c.release();
        assert_eq!(get(&root, "class/vtconsole/vtcon1/bind"), "1");
        assert_eq!(get(&root, "class/graphics/fbcon/cursor_blink"), "1");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dropping_the_guard_restores_the_console() {
        let root = sysfs();
        {
            let mut c = ConsoleTakeover::new(&root);
            c.engage();
            assert_eq!(get(&root, "class/vtconsole/vtcon1/bind"), "0");
        }
        assert_eq!(get(&root, "class/vtconsole/vtcon1/bind"), "1");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_already_unbound_console_is_not_rebound_by_release() {
        let root = sysfs();
        put(&root, "class/vtconsole/vtcon1/bind", "0\n");
        let mut c = ConsoleTakeover::new(&root);
        c.engage();
        c.release();
        assert_eq!(
            get(&root, "class/vtconsole/vtcon1/bind"),
            "0",
            "we only undo our own changes"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_machine_without_these_files_is_reported_not_fatal() {
        let root = tmp("empty");
        let mut c = ConsoleTakeover::new(&root);
        let notes = c.engage();
        assert!(notes.len() >= 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("cursor_blink")));
        c.release();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn release_twice_is_harmless() {
        let root = sysfs();
        let mut c = ConsoleTakeover::new(&root);
        c.engage();
        c.release();
        c.release();
        assert_eq!(get(&root, "class/vtconsole/vtcon1/bind"), "1");
        let _ = fs::remove_dir_all(root);
    }
}
