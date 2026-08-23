//! Finding executables, the way a shell would.

use std::path::{Path, PathBuf};

/// A `which`-alike: resolve a bare name against `PATH`, or take a path as given.
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let direct = Path::new(program);
    if direct.components().count() > 1 {
        return is_executable(direct).then(|| direct.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(program)).find(|c| is_executable(c))
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
