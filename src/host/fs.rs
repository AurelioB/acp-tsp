//! Confined filesystem access for ACP client `fs/*` requests.
use std::fmt::Write as _;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use agent_client_protocol::Error;
use agent_client_protocol::schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest, WriteTextFileResponse,
};

#[derive(Clone)]
pub struct FileSystem {
    roots: Vec<PathBuf>,
}

impl FileSystem {
    pub fn new(roots: Vec<PathBuf>) -> io::Result<Self> {
        Ok(Self {
            roots: roots
                .into_iter()
                .map(std::fs::canonicalize)
                .collect::<io::Result<_>>()?,
        })
    }

    fn resolve(&self, path: &Path) -> Result<PathBuf, Error> {
        if !path.is_absolute() {
            return Err(Error::invalid_params().data("file path must be absolute"));
        }
        // Probe the original path: collapsing `link/..` lexically before
        // resolving `link` changes which file the agent actually named.
        let mut ancestor = path;
        loop {
            match std::fs::symlink_metadata(ancestor) {
                Ok(_) => break,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    ancestor = ancestor.parent().ok_or_else(Error::invalid_params)?;
                }
                Err(err) => return Err(io_error(path, err)),
            }
        }
        let canonical = std::fs::canonicalize(ancestor).map_err(|err| io_error(path, err))?;
        let mut resolved = canonical;
        for component in path
            .strip_prefix(ancestor)
            .map_err(|_| Error::invalid_params())?
            .components()
        {
            match component {
                Component::Normal(name) => resolved.push(name),
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::CurDir => {}
                Component::RootDir | Component::Prefix(_) => return Err(Error::invalid_params()),
            }
        }
        if !self.roots.iter().any(|root| resolved.starts_with(root)) {
            return Err(Error::invalid_params().data("file path is outside session roots"));
        }
        Ok(resolved)
    }

    pub fn read(&self, request: ReadTextFileRequest) -> Result<ReadTextFileResponse, Error> {
        if request.line == Some(0) {
            return Err(Error::invalid_params().data("line must be 1-based"));
        }
        let path = self.resolve(&request.path)?;
        let content = std::fs::read_to_string(&path).map_err(|err| io_error(&request.path, err))?;
        if request.line.is_none() && request.limit.is_none() {
            return Ok(ReadTextFileResponse::new(content));
        }
        let start = request.line.unwrap_or(1) as usize - 1;
        let limit = request.limit.unwrap_or(u32::MAX) as usize;
        let selected = content
            .split_inclusive('\n')
            .skip(start)
            .take(limit)
            .collect::<String>();
        Ok(ReadTextFileResponse::new(selected))
    }

    pub fn write(&self, request: WriteTextFileRequest) -> Result<WriteTextFileResponse, Error> {
        let path = self.resolve(&request.path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| io_error(&request.path, err))?;
        }
        std::fs::write(path, request.content).map_err(|err| io_error(&request.path, err))?;
        Ok(WriteTextFileResponse::new())
    }
}

fn io_error(path: &Path, err: io::Error) -> Error {
    if err.kind() == io::ErrorKind::NotFound {
        let mut uri = String::from("file://");
        for byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
                uri.push(char::from(*byte));
            } else {
                write!(uri, "%{byte:02X}").expect("writing into String");
            }
        }
        Error::resource_not_found(Some(uri))
    } else {
        Error::internal_error().data(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn confines_reads_and_writes_including_missing_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), "private").unwrap();
        symlink(&outside, root.join("link")).unwrap();
        let fs = FileSystem::new(vec![root.clone()]).unwrap();
        let inside = root.join("new/deep/file");
        fs.write(WriteTextFileRequest::new("s", &inside, "a\nb\nc\n"))
            .unwrap();
        assert_eq!(
            fs.read(ReadTextFileRequest::new("s", &inside).line(2).limit(1))
                .unwrap()
                .content,
            "b\n"
        );
        for path in [
            root.join("../outside/secret"),
            root.join("link/secret"),
            root.join("link/new/file"),
            root.join("link/../secret"),
            root.join("missing/../../outside/secret"),
        ] {
            assert_eq!(
                fs.read(ReadTextFileRequest::new("s", &path))
                    .unwrap_err()
                    .code,
                Error::invalid_params().code
            );
            assert_eq!(
                fs.write(WriteTextFileRequest::new("s", &path, "overwrite"))
                    .unwrap_err()
                    .code,
                Error::invalid_params().code
            );
        }
        assert_eq!(
            std::fs::read_to_string(outside.join("secret")).unwrap(),
            "private"
        );
        let missing = fs
            .read(ReadTextFileRequest::new("s", root.join("not here")))
            .unwrap_err();
        assert_eq!(missing.code, Error::resource_not_found(None).code);
        assert!(
            missing.data.unwrap()["uri"]
                .as_str()
                .unwrap()
                .ends_with("/not%20here")
        );
        assert_eq!(
            fs.read(ReadTextFileRequest::new("s", inside).line(0))
                .unwrap_err()
                .code,
            Error::invalid_params().code
        );
    }
}
