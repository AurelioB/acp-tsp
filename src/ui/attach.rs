//! File mentions and a bounded workspace picker.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use super::Msg;
use agent_client_protocol::schema::v1::{
    AudioContent, ContentBlock, EmbeddedResource, EmbeddedResourceResource, ImageContent,
    PromptCapabilities, ResourceLink, TextContent, TextResourceContents,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use tern_sdk::ui::{self, Node, PickerItem};

const MAX_ENTRIES: usize = 4000;
const MAX_DEPTH: usize = 6;
const TEXT_LIMIT: u64 = 256 * 1024;
const MEDIA_LIMIT: u64 = 5 * 1024 * 1024;

#[derive(Default)]
pub struct AttachPicker {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub files: Vec<PathBuf>,
}

impl AttachPicker {
    pub fn open(&mut self, cwd: &Path) {
        self.files.clear();
        self.query.clear();
        self.selected = 0;
        self.open = true;
        let mut dirs = vec![(cwd.to_path_buf(), 0)];
        let mut visited = 0;
        while let Some((dir, depth)) = dirs.pop() {
            if depth > MAX_DEPTH || visited >= MAX_ENTRIES {
                continue;
            }
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > MAX_ENTRIES {
                    break;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || matches!(name.as_ref(), "target" | "node_modules") {
                    continue;
                }
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && depth < MAX_DEPTH {
                    dirs.push((path, depth + 1));
                } else if kind.is_file()
                    && let Ok(relative) = path.strip_prefix(cwd)
                {
                    self.files.push(relative.to_path_buf());
                }
            }
        }
        self.files.sort();
    }

    pub fn matches(&self) -> Vec<&PathBuf> {
        let query = self.query.to_lowercase();
        self.files
            .iter()
            .filter(|path| path.to_string_lossy().to_lowercase().contains(&query))
            .take(100)
            .collect()
    }

    pub fn view(&self) -> Node<Msg> {
        let matches = self.matches();
        let selected = self.selected.min(matches.len().saturating_sub(1));
        let items: Vec<PickerItem> = matches
            .iter()
            .map(|path| {
                let name = path.to_string_lossy().into_owned();
                PickerItem::new(name.clone(), name)
            })
            .collect();
        let mut picker = ui::picker()
            .key("attachments")
            .title("Attach file")
            .query(self.query.clone())
            .cursor(self.query.encode_utf16().count())
            .items(items)
            .empty("No matching files")
            .on_select(|id| Msg::AttachSelect(id.to_owned()));
        if let Some(path) = matches.get(selected) {
            picker = picker.selected(path.to_string_lossy().into_owned());
        }
        picker.into()
    }
}

fn mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "rs" => "text/x-rust",
        "md" => "text/markdown",
        "json" => "application/json",
        "toml" => "application/toml",
        "txt" | "log" | "csv" => "text/plain",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "html" => "text/html",
        "css" => "text/css",
        "yaml" | "yml" => "text/yaml",
        "sh" => "text/x-shellscript",
        _ => "application/octet-stream",
    }
}

fn file_uri(path: &Path) -> String {
    let mut result = String::from("file://");
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

pub fn mention(path: &Path) -> String {
    let mut result = String::new();
    for &byte in path.as_os_str().as_encoded_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{result} ")
}

fn decode_mention(mention: &str) -> io::Result<String> {
    let bytes = mention.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let Some(pair) = bytes.get(index + 1..index + 3) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid file mention",
                ));
            };
            let hex = std::str::from_utf8(pair)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid file mention")
                })?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid file mention"))
}
/// Resolve the actual file (including symlinks) inside one of the granted roots.
fn resolve(path: &str, cwd: &Path, roots: &[PathBuf]) -> io::Result<PathBuf> {
    let input = Path::new(path);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        cwd.join(input)
    };
    let file = candidate.canonicalize()?;
    if !file.is_file()
        || !std::iter::once(cwd)
            .chain(roots.iter().map(PathBuf::as_path))
            .filter_map(|root| root.canonicalize().ok())
            .any(|root| file.starts_with(root))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "attachment is outside session roots",
        ));
    }
    Ok(file)
}

/// Build a single text block and capability-gated resources, retaining mention text for the echo.
pub fn prompt(
    text: &str,
    cwd: &Path,
    roots: &[PathBuf],
    caps: Option<&PromptCapabilities>,
) -> io::Result<Vec<ContentBlock>> {
    let mut blocks = vec![ContentBlock::Text(TextContent::new(text))];
    for mention in text
        .split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
    {
        if mention.is_empty() {
            continue;
        }
        let mention = decode_mention(mention)?;
        let path = resolve(&mention, cwd, roots)?;
        let metadata = fs::metadata(&path)?;
        let uri = file_uri(&path);
        let mime_type = mime(&path);
        let mut link = ResourceLink::new(mention, &uri);
        link.mime_type = Some(mime_type.to_owned());
        link.size = i64::try_from(metadata.len()).ok();
        blocks.push(ContentBlock::ResourceLink(link));
        let can_image = mime_type.starts_with("image/") && caps.is_some_and(|c| c.image);
        let can_audio = mime_type.starts_with("audio/") && caps.is_some_and(|c| c.audio);
        if (can_image || can_audio) && metadata.len() <= MEDIA_LIMIT {
            let bytes = fs::read(&path)?;
            if can_image {
                blocks.push(ContentBlock::Image(
                    ImageContent::new(STANDARD.encode(bytes), mime_type).uri(uri),
                ));
            } else {
                blocks.push(ContentBlock::Audio(AudioContent::new(
                    STANDARD.encode(bytes),
                    mime_type,
                )));
            }
        } else if caps.is_some_and(|c| c.embedded_context)
            && metadata.len() <= TEXT_LIMIT
            && !mime_type.starts_with("image/")
            && !mime_type.starts_with("audio/")
        {
            let bytes = fs::read(&path)?;
            if let Ok(content) = String::from_utf8(bytes) {
                blocks.push(ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(
                        TextResourceContents::new(content, uri).mime_type(mime_type.to_owned()),
                    ),
                )));
            }
        }
    }
    Ok(blocks)
}
