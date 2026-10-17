pub use std::env::*;
use std::{ffi::OsString, path::PathBuf, sync::OnceLock};

static ARGS: OnceLock<Vec<String>> = OnceLock::new();

pub fn set_args(args: Vec<String>) {
    let _ = ARGS.set(args);
}
pub fn args() -> std::vec::IntoIter<String> {
    ARGS.get_or_init(|| {
        vec![
            "horizon-client".into(),
            "--resources".into(),
            "/resources".into(),
        ]
    })
    .clone()
    .into_iter()
}
pub fn args_os() -> std::vec::IntoIter<OsString> {
    args().map(OsString::from).collect::<Vec<_>>().into_iter()
}
pub fn current_dir() -> std::io::Result<PathBuf> {
    Ok(PathBuf::from("/"))
}
pub fn current_exe() -> std::io::Result<PathBuf> {
    Ok(PathBuf::from("/horizon-client"))
}
pub fn temp_dir() -> PathBuf {
    PathBuf::from("/tmp")
}
