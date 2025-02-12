use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, FileType, Metadata};
use std::path::Path;
use std::sync::Arc;

use crate::{ClientState, Error, ReadDirSpec, Result};

/// Representation of a file or directory.
///
/// This representation does not wrap a `std::fs::DirEntry`. Instead it copies
/// `file_name`, `file_type`, and optionally `metadata` out of the underlying
/// `std::fs::DirEntry`. This allows it to quickly drop the underlying file
/// descriptor.
pub struct DirEntry<C: ClientState> {
    /// Depth of this entry relative to the root directory where the walk
    /// started.
    pub depth: usize,
    /// File type for the file/directory that this entry points at.
    file_type: FileType,
    /// Field where clients can store state from within the The
    /// [`process_read_dir`](struct.WalkDirGeneric.html#method.process_read_dir)
    /// callback.
    pub client_state: C::DirEntryState,
    // True if [`follow_links`] is `true` AND was created from a symlink path.
    follow_link: bool,
    // Origins of symlinks followed to get to this entry.
    follow_link_ancestors: Arc<Vec<Arc<Path>>>,
    pub(crate) inner: DirEntryInner,
}

impl<C: ClientState> DirEntry<C> {
    pub(crate) fn from_entry(
        depth: usize,
        parent_path: &Arc<Path>,
        fs_dir_entry: &fs::DirEntry,
        follow_link_ancestors: Arc<Vec<Arc<Path>>>,
    ) -> Result<Self> {
        let file_type = fs_dir_entry
            .file_type()
            .map_err(|err| Error::from_path(depth, fs_dir_entry.path(), err))?;
        let inner = DirEntryInner::from_entry(parent_path, fs_dir_entry, file_type);
        Ok(DirEntry {
            depth,
            file_type,
            client_state: C::DirEntryState::default(),
            follow_link: false,
            follow_link_ancestors,
            inner,
        })
    }

    // Only used for root and when following links.
    pub(crate) fn from_path(
        depth: usize,
        path: &Path,
        follow_link: bool,
        follow_link_ancestors: Arc<Vec<Arc<Path>>>,
    ) -> Result<Self> {
        let metadata = if follow_link {
            fs::metadata(path).map_err(|err| Error::from_path(depth, path.to_owned(), err))?
        } else {
            fs::symlink_metadata(path)
                .map_err(|err| Error::from_path(depth, path.to_owned(), err))?
        };

        Ok(DirEntry {
            depth,
            file_type: metadata.file_type(),
            client_state: C::DirEntryState::default(),
            follow_link,
            follow_link_ancestors,
            inner: DirEntryInner::from_path(path, &metadata),
        })
    }

    /// Return the file type for the file that this entry points to.
    ///
    /// If this is a symbolic link and [`follow_links`] is `true`, then this
    /// returns the type of the target.
    ///
    /// This never makes any system calls.
    ///
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// Return the file name of this entry.
    ///
    /// If this entry has no file name (e.g., `/`), then the full path is
    /// returned.
    pub fn file_name(&self) -> &OsStr {
        &self.inner.file_name()
    }

    /// Returns the depth at which this entry was created relative to the root.
    ///
    /// The smallest depth is `0` and always corresponds to the path given
    /// to the `new` function on `WalkDir`. Its direct descendants have depth
    /// `1`, and their descendants have depth `2`, and so on.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Path to the file/directory represented by this entry.
    ///
    /// The path is created by joining `parent_path` with `file_name`.
    pub fn path(&self) -> Cow<'_, Path> {
        self.inner.path()
    }

    /// Returns `true` if and only if this entry was created from a symbolic
    /// link. This is unaffected by the [`follow_links`] setting.
    ///
    /// When `true`, the value returned by the [`path`] method is a
    /// symbolic link name. To get the full target path, you must call
    /// [`std::fs::read_link(entry.path())`].
    ///
    /// [`path`]: struct.DirEntry.html#method.path
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    /// [`std::fs::read_link(entry.path())`]: https://doc.rust-lang.org/stable/std/fs/fn.read_link.html
    pub fn path_is_symlink(&self) -> bool {
        self.file_type.is_symlink() || self.follow_link
    }

    /// Return the metadata for the file that this entry points to.
    ///
    /// This will follow symbolic links if and only if the [`WalkDir`] value
    /// has [`follow_links`] enabled.
    ///
    /// # Platform behavior
    ///
    /// This calls [`std::fs::symlink_metadata`] if [`follow_links`] is disabled.
    ///
    /// If [`follow_links`] is enabled, then [`std::fs::metadata`] is called instead.
    ///
    /// # Errors
    ///
    /// Similar to [`std::fs::metadata`], returns errors for path values that
    /// the program does not have permissions to access or if the path does not
    /// exist.
    ///
    /// [`WalkDir`]: struct.WalkDir.html
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    /// [`std::fs::metadata`]: https://doc.rust-lang.org/std/fs/fn.metadata.html
    /// [`std::fs::symlink_metadata`]: https://doc.rust-lang.org/stable/std/fs/fn.symlink_metadata.html
    pub fn metadata(&self) -> Result<fs::Metadata> {
        if self.follow_link {
            fs::metadata(self.path())
        } else {
            fs::symlink_metadata(self.path())
        }
        .map_err(|err| Error::from_entry(self, err))
    }

    /// Reference to the path of the directory containing this entry.
    pub fn parent_path(&self) -> Option<&Path> {
        self.inner.parent_path()
    }

    /// Set whether or not to read the contents of this directory.
    ///
    /// By default, `WalkDir` reads the contents of directories. If you want to
    /// skip a directory, you can set this field to `false` in the
    /// [`process_read_dir`](struct.WalkDirGeneric.html#method.process_read_dir)
    /// callback.
    ///
    /// This has no effect on non-directory entries.
    pub fn set_read_children(&mut self, read_children_: bool) {
        self.inner.set_read_children(read_children_);
    }

    /// If `read_children` is set and resulting `fs::read_dir` generates an error
    /// then you can get the error here.
    ///
    /// This will always return `None` on non-directory entries.
    pub fn read_children_error(&self) -> Option<&Error> {
        match &self.inner {
            DirEntryInner::Dir {
                read_children_error,
                ..
            } => read_children_error.as_ref().map(|e| e.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn read_children_spec(
        &self,
        client_read_state: C::ReadDirState,
    ) -> Option<ReadDirSpec<C>> {
        match &self.inner {
            DirEntryInner::Dir {
                path,
                read_children: true,
                ..
            } => Some(ReadDirSpec {
                depth: self.depth,
                client_read_state,
                path: path.clone(),
                follow_link_ancestors: self.follow_link_ancestors.clone(),
            }),
            _ => None,
        }
    }

    pub(crate) fn follow_symlink(&self) -> Result<Self> {
        let path = self.path();
        let origins = self.follow_link_ancestors.clone();
        let dir_entry = DirEntry::from_path(self.depth, &path, true, origins)?;

        if dir_entry.file_type.is_dir() {
            let target = fs::read_link(&path).map_err(|err| Error::from_io(self.depth, err))?;
            for ancestor in self.follow_link_ancestors.iter().rev() {
                if target.as_path() == ancestor.as_ref() {
                    return Err(Error::from_loop(
                        self.depth,
                        ancestor.as_ref(),
                        path.as_ref(),
                    ));
                }
            }
        }

        Ok(dir_entry)
    }
}

impl<C: ClientState> fmt::Debug for DirEntry<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DirEntry({:?})", self.path())
    }
}

pub(crate) enum DirEntryInner {
    /// Not necessarily a real directory,
    /// can also be a symlink to a directory.
    /// Anything we can read its children.
    Dir {
        /// referenced by children to avoid cloning the path.
        path: Arc<Path>,
        /// Either to read child entries. This is automatically set
        /// to `true` for directories. The
        /// [`process_read_dir`](struct.WalkDirGeneric.html#method.process_read_dir) callback
        /// may set this field to `false` to skip reading the contents of a
        /// particular directory.
        read_children: bool,
        /// If `read_children` is set and resulting `fs::read_dir` generates an error
        /// then that error is stored here.
        read_children_error: Option<Box<Error>>,
    },
    Other {
        file_name: Box<OsStr>,
        parent_path: Option<Arc<Path>>,
    },
}

impl DirEntryInner {
    pub(crate) fn from_entry(
        parent_path: &Arc<Path>,
        fs_dir_entry: &fs::DirEntry,
        file_type: FileType,
    ) -> Self {
        if file_type.is_dir() {
            DirEntryInner::Dir {
                path: Arc::from(fs_dir_entry.path()),
                read_children: true,
                read_children_error: None,
            }
        } else {
            DirEntryInner::Other {
                file_name: fs_dir_entry.file_name().into(),
                parent_path: Some(parent_path.clone()),
            }
        }
    }
    pub(crate) fn from_path(path: &Path, metadata: &Metadata) -> Self {
        if metadata.file_type().is_dir() {
            DirEntryInner::Dir {
                path: Arc::from(path),
                read_children: true,
                read_children_error: None,
            }
        } else {
            DirEntryInner::Other {
                file_name: path.file_name().unwrap_or(path.as_os_str()).into(),
                parent_path: path.parent().map(Into::into),
            }
        }
    }
    pub(crate) fn file_name(&self) -> &OsStr {
        match self {
            DirEntryInner::Dir { path, .. } => path.file_name().unwrap_or(path.as_os_str()),
            DirEntryInner::Other { file_name, .. } => file_name.as_ref(),
        }
    }

    pub(crate) fn path(&self) -> Cow<'_, Path> {
        match self {
            DirEntryInner::Dir { path, .. } => path.as_ref().into(),
            DirEntryInner::Other {
                parent_path,
                file_name,
            } => match parent_path {
                Some(parent) => parent.join(file_name.as_ref()).into(),
                None => AsRef::<Path>::as_ref(file_name.as_ref()).into(),
            },
        }
    }

    pub(crate) fn parent_path(&self) -> Option<&Path> {
        match self {
            DirEntryInner::Dir { path, .. } => path.parent(),
            DirEntryInner::Other { parent_path, .. } => parent_path.as_deref(),
        }
    }

    pub(crate) fn set_read_children(&mut self, read_children_: bool) {
        if let DirEntryInner::Dir {
            ref mut read_children,
            ..
        } = self
        {
            *read_children = read_children_;
        }
    }
}
