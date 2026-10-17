//! Shared-memory browser filesystem. Resources are immutable after preload.
//! Writes are immediately visible to every handle; `drain_changes` transfers
//! pending mutable changes to the browser's asynchronous persistence adapter.
//! `sync_all` synchronizes this VFS, not an OPFS transaction: the caller must
//! await persistence separately before promising durable browser storage.
use crate::{
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};
pub use std::fs::TryLockError;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, LazyLock, Weak},
};

const MAX_MUTABLE_FILE: usize = 256 * 1024 * 1024;
fn error(kind: io::ErrorKind, message: &str) -> io::Error {
    io::Error::new(kind, message)
}
fn guard<T>(value: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    value
        .lock()
        .map_err(|_| io::Error::other("browser filesystem lock poisoned"))
}
fn normalize(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(error(io::ErrorKind::InvalidInput, "NUL in path"));
    }
    let mut result = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => result.push(name),
            Component::ParentDir if result != Path::new("/") => {
                result.pop();
            }
            Component::ParentDir | Component::Prefix(_) => {
                return Err(error(
                    io::ErrorKind::PermissionDenied,
                    "path escapes virtual root",
                ));
            }
        }
    }
    Ok(result)
}
fn resource(path: &Path) -> bool {
    path.starts_with("/resources")
}
fn mutable(path: &Path) -> io::Result<()> {
    if resource(path) || path == Path::new("/") {
        Err(error(
            io::ErrorKind::PermissionDenied,
            "immutable filesystem path",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Permissions {
    readonly: bool,
}
impl Permissions {
    pub fn readonly(&self) -> bool {
        self.readonly
    }
    pub fn set_readonly(&mut self, readonly: bool) {
        self.readonly = readonly;
    }
}
#[derive(Clone, Copy, Debug)]
pub struct FileType {
    directory: bool,
}
impl FileType {
    pub fn is_file(&self) -> bool {
        !self.directory
    }
    pub fn is_dir(&self) -> bool {
        self.directory
    }
    pub fn is_symlink(&self) -> bool {
        false
    }
}
#[derive(Clone, Debug)]
pub struct Metadata {
    len: u64,
    kind: FileType,
    permissions: Permissions,
    modified: SystemTime,
    accessed: SystemTime,
    created: SystemTime,
}
impl Metadata {
    pub fn is_symlink(&self) -> bool {
        false
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_file(&self) -> bool {
        self.kind.is_file()
    }
    pub fn is_dir(&self) -> bool {
        self.kind.is_dir()
    }
    pub fn file_type(&self) -> FileType {
        self.kind
    }
    pub fn permissions(&self) -> Permissions {
        self.permissions.clone()
    }
    pub fn modified(&self) -> io::Result<SystemTime> {
        Ok(self.modified)
    }
    pub fn accessed(&self) -> io::Result<SystemTime> {
        Ok(self.accessed)
    }
    pub fn created(&self) -> io::Result<SystemTime> {
        Ok(self.created)
    }
}
#[derive(Debug)]
struct Node {
    bytes: Option<Arc<Vec<u8>>>,
    names: BTreeSet<PathBuf>,
    meta: Metadata,
    owner: Weak<Handle>,
}
impl Node {
    fn new(path: PathBuf, bytes: Option<Arc<Vec<u8>>>) -> Self {
        let now = SystemTime::now();
        Self {
            meta: Metadata {
                len: bytes.as_ref().map_or(0, |b| b.len() as u64),
                kind: FileType {
                    directory: bytes.is_none(),
                },
                permissions: Permissions {
                    readonly: resource(&path),
                },
                modified: now,
                accessed: now,
                created: now,
            },
            bytes,
            names: BTreeSet::from([path]),
            owner: Weak::new(),
        }
    }
}
struct Vfs {
    nodes: BTreeMap<PathBuf, Arc<Mutex<Node>>>,
    dirty: BTreeSet<PathBuf>,
}
static VFS: LazyLock<Mutex<Vfs>> = LazyLock::new(|| {
    let root = PathBuf::from("/");
    Mutex::new(Vfs {
        nodes: BTreeMap::from([(root.clone(), Arc::new(Mutex::new(Node::new(root, None))))]),
        dirty: BTreeSet::new(),
    })
});
impl Vfs {
    fn node(&self, path: &Path) -> io::Result<Arc<Mutex<Node>>> {
        self.nodes
            .get(path)
            .cloned()
            .ok_or_else(|| error(io::ErrorKind::NotFound, "virtual path does not exist"))
    }
    fn directory(&self, path: &Path) -> io::Result<()> {
        if guard(&*self.node(path)?)?.meta.is_dir() {
            Ok(())
        } else {
            Err(error(io::ErrorKind::NotADirectory, "not a directory"))
        }
    }
    fn parent(&self, path: &Path) -> io::Result<()> {
        self.directory(path.parent().unwrap_or(Path::new("/")))
    }
    fn parents(&mut self, path: &Path, preload: bool) -> io::Result<()> {
        let mut current = PathBuf::from("/");
        for part in path.components().skip(1) {
            current.push(part.as_os_str());
            if self.nodes.contains_key(&current) {
                self.directory(&current)?;
                continue;
            }
            if !preload {
                mutable(&current)?;
                self.dirty.insert(current.clone());
            }
            self.nodes.insert(
                current.clone(),
                Arc::new(Mutex::new(Node::new(current.clone(), None))),
            );
        }
        Ok(())
    }
    fn unlink(&mut self, path: &Path) -> io::Result<()> {
        let node = self.node(path)?;
        guard(&node)?.names.remove(path);
        self.nodes.remove(path);
        self.dirty.insert(path.to_owned());
        Ok(())
    }
}

/// Bootstrap/import only: installs bytes, creating parents even under /resources.
/// Does not queue persistence; use normal writes for newly changed mutable data.
pub fn preload(path: impl AsRef<Path>, bytes: impl Into<Vec<u8>>) -> io::Result<()> {
    let path = normalize(path.as_ref())?;
    if path == Path::new("/") {
        return Err(error(io::ErrorKind::IsADirectory, "cannot preload root"));
    }
    let mut fs = guard(&VFS)?;
    fs.parents(path.parent().unwrap(), true)?;
    let bytes = Arc::new(bytes.into());
    if let Some(node) = fs.nodes.get(&path) {
        let mut node = guard(node)?;
        if node.meta.is_dir() {
            return Err(error(
                io::ErrorKind::IsADirectory,
                "cannot preload a directory",
            ));
        }
        node.meta.len = bytes.len() as u64;
        node.meta.modified = SystemTime::now();
        node.bytes = Some(bytes);
    } else {
        fs.nodes.insert(
            path.clone(),
            Arc::new(Mutex::new(Node::new(path, Some(bytes)))),
        );
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Change {
    /// Upsert this file, replacing a directory at the same path if necessary.
    Write { path: PathBuf, bytes: Arc<Vec<u8>> },
    /// Ensure this directory exists, replacing a file at the same path.
    CreateDir { path: PathBuf },
    /// Remove a missing/stale entry. Persistence should tolerate an absent path.
    Remove { path: PathBuf },
}
fn changes(fs: &Vfs, paths: impl Iterator<Item = PathBuf>) -> io::Result<Vec<Change>> {
    let mut result: Vec<_> = paths
        .filter(|p| !resource(p) && p != Path::new("/"))
        .map(|path| {
            Ok(if let Some(node) = fs.nodes.get(&path) {
                match &guard(node)?.bytes {
                    Some(bytes) => Change::Write {
                        path,
                        bytes: bytes.clone(),
                    },
                    None => Change::CreateDir { path },
                }
            } else {
                Change::Remove { path }
            })
        })
        .collect::<io::Result<_>>()?;
    // A rename or recursive delete must remove children before their parent.
    // Directories for new files must be created before any file bytes are saved.
    result.sort_by_key(|change| match change {
        Change::Remove { path } => (0, usize::MAX - path.components().count(), path.clone()),
        Change::CreateDir { path } => (1, path.components().count(), path.clone()),
        Change::Write { path, .. } => (2, path.components().count(), path.clone()),
    });
    Ok(result)
}
/// All mutable directories/files, for a complete persistence checkpoint.
pub fn snapshot() -> io::Result<Vec<Change>> {
    let fs = guard(&VFS)?;
    changes(&fs, fs.nodes.keys().cloned())
}
/// Atomically takes pending changes. A failed persistence attempt must retry the
/// returned batch before draining a later one, or write a fresh full snapshot.
pub fn drain_changes() -> io::Result<Vec<Change>> {
    let mut fs = guard(&VFS)?;
    let result = changes(&fs, fs.dirty.iter().cloned())?;
    fs.dirty.clear();
    Ok(result)
}

pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    let path = normalize(path.as_ref())?;
    Ok(guard(&*guard(&VFS)?.node(&path)?)?.meta.clone())
}
pub fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    metadata(path)
}
pub fn exists(path: impl AsRef<Path>) -> io::Result<bool> {
    match metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let path = normalize(path.as_ref())?;
    metadata(&path)?;
    Ok(path)
}
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut out = Vec::new();
    (&file).read_to_end(&mut out)?;
    Ok(out)
}
pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|e| error(io::ErrorKind::InvalidData, &e.to_string()))
}
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    File::create(path)?.write_all(contents.as_ref())
}
pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref())?;
    mutable(&path)?;
    let mut fs = guard(&VFS)?;
    fs.parent(&path)?;
    if fs.nodes.contains_key(&path) {
        return Err(error(io::ErrorKind::AlreadyExists, "path already exists"));
    }
    fs.nodes.insert(
        path.clone(),
        Arc::new(Mutex::new(Node::new(path.clone(), None))),
    );
    fs.dirty.insert(path);
    Ok(())
}
pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref())?;
    let mut fs = guard(&VFS)?;
    if fs.nodes.contains_key(&path) {
        return fs.directory(&path);
    }
    mutable(&path)?;
    fs.parents(&path, false)
}
pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = normalize(path.as_ref())?;
    mutable(&path)?;
    let mut fs = guard(&VFS)?;
    if guard(&*fs.node(&path)?)?.meta.is_dir() {
        return Err(error(
            io::ErrorKind::IsADirectory,
            "cannot remove directory as file",
        ));
    }
    fs.unlink(&path)
}
fn remove_directory(path: &Path, recursive: bool) -> io::Result<()> {
    let path = normalize(path)?;
    mutable(&path)?;
    let mut fs = guard(&VFS)?;
    fs.directory(&path)?;
    let children: Vec<_> = fs
        .nodes
        .keys()
        .filter(|p| *p != &path && p.starts_with(&path))
        .cloned()
        .collect();
    if !recursive && !children.is_empty() {
        return Err(error(
            io::ErrorKind::DirectoryNotEmpty,
            "directory not empty",
        ));
    }
    for child in children {
        mutable(&child)?;
        fs.unlink(&child)?;
    }
    fs.unlink(&path)
}
pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    remove_directory(path.as_ref(), false)
}
pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    remove_directory(path.as_ref(), true)
}
pub fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let bytes = read(from)?;
    write(to, &bytes)?;
    Ok(bytes.len() as u64)
}
pub fn hard_link(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let from = normalize(from.as_ref())?;
    let to = normalize(to.as_ref())?;
    mutable(&from)?;
    mutable(&to)?;
    let mut fs = guard(&VFS)?;
    let node = fs.node(&from)?;
    fs.parent(&to)?;
    if fs.nodes.contains_key(&to) {
        return Err(error(io::ErrorKind::AlreadyExists, "link target exists"));
    }
    {
        let mut node = guard(&node)?;
        if node.meta.is_dir() {
            return Err(error(
                io::ErrorKind::PermissionDenied,
                "cannot hard-link directories",
            ));
        }
        node.names.insert(to.clone());
    }
    fs.nodes.insert(to.clone(), node);
    fs.dirty.insert(to);
    Ok(())
}
pub fn read_link(_path: impl AsRef<Path>) -> io::Result<PathBuf> {
    Err(error(
        io::ErrorKind::Unsupported,
        "browser VFS does not support symbolic links",
    ))
}
pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let from = normalize(from.as_ref())?;
    let to = normalize(to.as_ref())?;
    mutable(&from)?;
    mutable(&to)?;
    let mut fs = guard(&VFS)?;
    let node = fs.node(&from)?;
    if from == to {
        return Ok(());
    }
    fs.parent(&to)?;
    let directory = guard(&node)?.meta.is_dir();
    if directory && to.starts_with(&from) {
        return Err(error(
            io::ErrorKind::InvalidInput,
            "cannot move directory inside itself",
        ));
    }
    if let Some(target) = fs.nodes.get(&to) {
        if Arc::ptr_eq(target, &node) {
            return Ok(());
        }
        let target_dir = guard(target)?.meta.is_dir();
        if directory != target_dir {
            return Err(error(io::ErrorKind::InvalidInput, "rename types differ"));
        }
        if target_dir && fs.nodes.keys().any(|p| p != &to && p.starts_with(&to)) {
            return Err(error(
                io::ErrorKind::DirectoryNotEmpty,
                "rename target is not empty",
            ));
        }
        fs.unlink(&to)?;
    }
    let paths: Vec<_> = fs
        .nodes
        .keys()
        .filter(|p| *p == &from || (directory && p.starts_with(&from)))
        .cloned()
        .collect();
    for old in paths {
        let new = to.join(old.strip_prefix(&from).unwrap());
        let node = fs.nodes.remove(&old).unwrap();
        {
            let mut node = guard(&node)?;
            node.names.remove(&old);
            node.names.insert(new.clone());
        }
        fs.nodes.insert(new.clone(), node);
        fs.dirty.insert(old);
        fs.dirty.insert(new);
    }
    Ok(())
}
pub fn set_permissions(path: impl AsRef<Path>, permissions: Permissions) -> io::Result<()> {
    let path = normalize(path.as_ref())?;
    mutable(&path)?;
    let mut fs = guard(&VFS)?;
    guard(&*fs.node(&path)?)?.meta.permissions = permissions;
    fs.dirty.insert(path);
    Ok(())
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    path: PathBuf,
}
impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn file_name(&self) -> OsString {
        self.path.file_name().unwrap_or_default().to_owned()
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        metadata(&self.path)
    }
    pub fn file_type(&self) -> io::Result<FileType> {
        Ok(self.metadata()?.file_type())
    }
}
pub type ReadDir = std::vec::IntoIter<io::Result<DirEntry>>;
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let path = normalize(path.as_ref())?;
    let fs = guard(&VFS)?;
    fs.directory(&path)?;
    Ok(fs
        .nodes
        .keys()
        .filter(|p| p.as_path() != Path::new("/") && p.parent() == Some(path.as_path()))
        .map(|p| Ok(DirEntry { path: p.clone() }))
        .collect::<Vec<_>>()
        .into_iter())
}

#[derive(Debug)]
struct Handle {
    node: Arc<Mutex<Node>>,
    cursor: Mutex<u64>,
    readable: bool,
    writable: bool,
    append: bool,
}
#[derive(Debug)]
pub struct File {
    handle: Arc<Handle>,
}
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}
impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn read(&mut self, value: bool) -> &mut Self {
        self.read = value;
        self
    }
    pub fn write(&mut self, value: bool) -> &mut Self {
        self.write = value;
        self
    }
    pub fn append(&mut self, value: bool) -> &mut Self {
        self.append = value;
        self
    }
    pub fn truncate(&mut self, value: bool) -> &mut Self {
        self.truncate = value;
        self
    }
    pub fn create(&mut self, value: bool) -> &mut Self {
        self.create = value;
        self
    }
    pub fn create_new(&mut self, value: bool) -> &mut Self {
        self.create_new = value;
        self
    }
    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let writable = self.write || self.append;
        if (!self.read && !writable)
            || ((self.truncate || self.create || self.create_new) && !writable)
            || (self.truncate && self.append && !self.create_new)
        {
            return Err(error(io::ErrorKind::InvalidInput, "invalid open options"));
        }
        let path = normalize(path.as_ref())?;
        if writable {
            mutable(&path)?;
        }
        let mut fs = guard(&VFS)?;
        let node = match fs.nodes.get(&path).cloned() {
            Some(_) if self.create_new => {
                return Err(error(io::ErrorKind::AlreadyExists, "path already exists"));
            }
            Some(node) => node,
            None if self.create || self.create_new => {
                fs.parent(&path)?;
                let node = Arc::new(Mutex::new(Node::new(
                    path.clone(),
                    Some(Arc::new(Vec::new())),
                )));
                fs.nodes.insert(path.clone(), node.clone());
                fs.dirty.insert(path);
                node
            }
            None => {
                return Err(error(
                    io::ErrorKind::NotFound,
                    "virtual file does not exist",
                ));
            }
        };
        {
            let mut value = guard(&node)?;
            if value.meta.is_dir() {
                return Err(error(
                    io::ErrorKind::IsADirectory,
                    "cannot open directory as file",
                ));
            }
            if writable && value.meta.permissions.readonly {
                return Err(error(io::ErrorKind::PermissionDenied, "read-only file"));
            }
            if self.truncate && !self.create_new {
                value.bytes = Some(Arc::new(Vec::new()));
                value.meta.len = 0;
                value.meta.modified = SystemTime::now();
                fs.dirty.extend(value.names.iter().cloned());
            }
        }
        Ok(File {
            handle: Arc::new(Handle {
                node,
                cursor: Mutex::new(0),
                readable: self.read,
                writable,
                append: self.append,
            }),
        })
    }
}
impl File {
    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        OpenOptions::new().read(true).open(path)
    }
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    }
    pub fn create_new(path: impl AsRef<Path>) -> io::Result<Self> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            handle: self.handle.clone(),
        })
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        Ok(guard(&self.handle.node)?.meta.clone())
    }
    pub fn sync_all(&self) -> io::Result<()> {
        drop(guard(&self.handle.node)?);
        Ok(())
    }
    pub fn sync_data(&self) -> io::Result<()> {
        self.sync_all()
    }
    pub fn set_len(&self, size: u64) -> io::Result<()> {
        if !self.handle.writable {
            return Err(error(
                io::ErrorKind::PermissionDenied,
                "file not open for writing",
            ));
        }
        let size = usize::try_from(size)
            .map_err(|_| error(io::ErrorKind::InvalidInput, "file length exceeds platform"))?;
        let mut fs = guard(&VFS)?;
        let mut node = guard(&self.handle.node)?;
        writable_node(&node)?;
        resize(&mut node, size)?;
        fs.dirty.extend(node.names.iter().cloned());
        Ok(())
    }
    pub fn try_lock(&self) -> Result<(), TryLockError> {
        let mut node = guard(&self.handle.node).map_err(TryLockError::Error)?;
        if node
            .owner
            .upgrade()
            .is_some_and(|owner| !Arc::ptr_eq(&owner, &self.handle))
        {
            return Err(TryLockError::WouldBlock);
        }
        node.owner = Arc::downgrade(&self.handle);
        Ok(())
    }
    pub fn lock(&self) -> io::Result<()> {
        self.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => error(
                io::ErrorKind::WouldBlock,
                "contended blocking file locks are unavailable in the browser",
            ),
            TryLockError::Error(e) => e,
        })
    }
    pub fn unlock(&self) -> io::Result<()> {
        let mut node = guard(&self.handle.node)?;
        if node
            .owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, &self.handle))
        {
            node.owner = Weak::new();
        }
        Ok(())
    }
}
fn writable_node(node: &Node) -> io::Result<()> {
    if node.meta.permissions.readonly || node.names.iter().any(|p| resource(p)) {
        Err(error(io::ErrorKind::PermissionDenied, "read-only file"))
    } else {
        Ok(())
    }
}
fn resize(node: &mut Node, size: usize) -> io::Result<()> {
    if size > MAX_MUTABLE_FILE {
        return Err(error(
            io::ErrorKind::OutOfMemory,
            "browser file exceeds 256 MiB limit",
        ));
    }
    let bytes = Arc::make_mut(node.bytes.as_mut().unwrap());
    bytes
        .try_reserve(size.saturating_sub(bytes.len()))
        .map_err(|e| error(io::ErrorKind::OutOfMemory, &e.to_string()))?;
    bytes.resize(size, 0);
    node.meta.len = size as u64;
    node.meta.modified = SystemTime::now();
    Ok(())
}
impl Read for &File {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.handle.readable {
            return Err(error(
                io::ErrorKind::PermissionDenied,
                "file not open for reading",
            ));
        }
        let mut cursor = guard(&self.handle.cursor)?;
        let mut node = guard(&self.handle.node)?;
        let bytes = node.bytes.as_ref().unwrap();
        let start = usize::try_from(*cursor)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let count = output.len().min(bytes.len() - start);
        output[..count].copy_from_slice(&bytes[start..start + count]);
        *cursor += count as u64;
        node.meta.accessed = SystemTime::now();
        Ok(count)
    }
}
impl Write for &File {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if !self.handle.writable {
            return Err(error(
                io::ErrorKind::PermissionDenied,
                "file not open for writing",
            ));
        }
        let mut fs = guard(&VFS)?;
        let mut cursor = guard(&self.handle.cursor)?;
        let mut node = guard(&self.handle.node)?;
        writable_node(&node)?;
        if input.is_empty() {
            return Ok(0);
        }
        let start = if self.handle.append {
            node.meta.len
        } else {
            *cursor
        };
        let start = usize::try_from(start)
            .map_err(|_| error(io::ErrorKind::InvalidInput, "offset exceeds platform"))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| error(io::ErrorKind::InvalidInput, "file offset overflow"))?;
        let size = end.max(node.meta.len as usize);
        resize(&mut node, size)?;
        Arc::make_mut(node.bytes.as_mut().unwrap())[start..end].copy_from_slice(input);
        *cursor = end as u64;
        fs.dirty.extend(node.names.iter().cloned());
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}
impl Seek for &File {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let mut cursor = guard(&self.handle.cursor)?;
        let offset = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(*cursor) + i128::from(value),
            SeekFrom::End(value) => {
                i128::from(guard(&self.handle.node)?.meta.len) + i128::from(value)
            }
        };
        *cursor = u64::try_from(offset)
            .map_err(|_| error(io::ErrorKind::InvalidInput, "invalid seek offset"))?;
        Ok(*cursor)
    }
}
impl Read for File {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        (&*self).read(output)
    }
}
impl Write for File {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        (&*self).write(input)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}
impl Seek for File {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        (&*self).seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coherent_handles_links_rename_and_readonly_resources() {
        create_dir_all("/vfs-test/save").unwrap();
        let mut first = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open("/vfs-test/save/region")
            .unwrap();
        first.write_all(b"world").unwrap();
        let mut cloned = first.try_clone().unwrap();
        cloned.seek(SeekFrom::Start(1)).unwrap();
        first.write_all(b"A").unwrap();
        assert_eq!(read("/vfs-test/save/region").unwrap(), b"wArld");
        hard_link("/vfs-test/save/region", "/vfs-test/save/link").unwrap();
        rename("/vfs-test/save", "/vfs-test/moved").unwrap();
        first.set_len(3).unwrap();
        assert_eq!(read("/vfs-test/moved/link").unwrap(), b"wAr");
        remove_file("/vfs-test/moved/region").unwrap();
        first.seek(SeekFrom::Start(0)).unwrap();
        first.write_all(b"X").unwrap();
        assert_eq!(read("/vfs-test/moved/link").unwrap(), b"XAr");
        assert!(matches!(
            read_dir("/vfs-test/moved").unwrap().next(),
            Some(Ok(_))
        ));
        preload("/resources/vfs-test", b"immutable".to_vec()).unwrap();
        assert_eq!(
            write("/resources/vfs-test", b"bad").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(read("../../escape").is_err());
        assert!(drain_changes().unwrap().iter().any(
            |c| matches!(c, Change::Write { path, .. } if path == Path::new("/vfs-test/moved/link"))
        ));
        first.try_lock().unwrap();
        let contender = File::open("/vfs-test/moved/link").unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        drop(first);
        assert!(matches!(
            contender.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        drop(cloned);
        contender.try_lock().unwrap();
        remove_dir_all("/vfs-test").unwrap();
        let removed: Vec<_> = drain_changes()
            .unwrap()
            .into_iter()
            .filter_map(|change| {
                if let Change::Remove { path } = change {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(removed.last().unwrap(), Path::new("/vfs-test"));
    }
}
