//! Enhanced paste (kitty's OSC 5522 clipboard protocol, `CSI ? 5522 h`): with it on, Tern sends
//! a paste as clipboard packets naming the MIME types on offer, and the program asks for the
//! one it wants. That is how an image pasted in the window reaches a program, even one running
//! on another host. omp does the same (`pi-coding-agent/src/utils/enhanced-paste.ts`).
//!
//! [`PasteFilter`] takes the packets out of the pty input. A text paste goes back into the
//! input as an ordinary bracketed paste, so every existing paste path keeps working; an image
//! is queued for the UI, which is woken by a synthetic event.
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};

pub const ENABLE: &[u8] = b"\x1b[?5522h";
pub const DISABLE: &[u8] = b"\x1b[?5522l";

const PREFIX: &[u8] = b"\x1b]5522;";
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const TEXT: &str = "text/plain";
/// kitty's "list the MIME types" target.
const LISTING: &str = ".";
/// One unfinished packet larger than this is not a clipboard packet worth keeping.
const MAX_PACKET: usize = 1 << 20;

/// A pasted image, its bytes bounded by `limit`.
#[derive(Debug, PartialEq, Eq)]
pub struct PastedImage {
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// What the filter produced from one chunk of input.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Filtered {
    /// Input to hand on, packets removed (text pastes re-encoded as bracketed paste).
    pub input: Vec<u8>,
    /// Requests to write back to the terminal.
    pub replies: Vec<Vec<u8>>,
    /// Finished image pastes.
    pub images: Vec<PastedImage>,
    /// Failures to show the user.
    pub errors: Vec<String>,
}

enum State {
    Idle,
    Listing {
        mimes: Vec<String>,
        dot_payload: bool,
        pw: Option<String>,
        loc: Option<String>,
    },
    Reading {
        mime: String,
        image: bool,
        data: Vec<u8>,
        too_big: bool,
    },
}

pub struct PasteFilter {
    /// A packet still being received.
    pending: Option<Vec<u8>>,
    /// Input ending in a possible packet start, kept for the next chunk.
    held: Vec<u8>,
    state: State,
    /// Largest decoded paste kept.
    limit: usize,
}

impl PasteFilter {
    pub fn new(limit: usize) -> Self {
        Self {
            pending: None,
            held: Vec::new(),
            state: State::Idle,
            limit,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Filtered {
        let mut out = Filtered::default();
        let joined;
        let mut rest = if self.held.is_empty() {
            bytes
        } else {
            let mut held = std::mem::take(&mut self.held);
            held.extend_from_slice(bytes);
            joined = held;
            &joined[..]
        };
        if let Some(pending) = &mut self.pending {
            // An ST terminator split across reads.
            if pending.last() == Some(&0x1b) && rest.first() == Some(&b'\\') {
                pending.pop();
                let packet = std::mem::take(pending);
                self.pending = None;
                self.packet(&packet, &mut out);
                rest = &rest[1..];
                return self.feed_rest(rest, out);
            }
            match terminator(rest) {
                Some((end, skip)) => {
                    let mut packet = std::mem::take(pending);
                    self.pending = None;
                    packet.extend_from_slice(&rest[..end]);
                    self.packet(&packet, &mut out);
                    rest = &rest[end + skip..];
                }
                None => {
                    pending.extend_from_slice(rest);
                    if pending.len() > MAX_PACKET + PREFIX.len() {
                        self.pending = None;
                        self.state = State::Idle;
                        out.errors.push("Paste was too large".into());
                    }
                    return out;
                }
            }
        }
        self.feed_rest(rest, out)
    }

    fn feed_rest(&mut self, mut rest: &[u8], mut out: Filtered) -> Filtered {
        while let Some(start) = find(rest, PREFIX) {
            out.input.extend_from_slice(&rest[..start]);
            let body = &rest[start + PREFIX.len()..];
            match terminator(body) {
                Some((end, skip)) => {
                    self.packet(&body[..end], &mut out);
                    rest = &body[end + skip..];
                }
                None => {
                    self.pending = Some(body.to_vec());
                    return out;
                }
            }
        }
        // A packet prefix split across reads: hold back a tail that could start one. The
        // caller flushes it if nothing follows soon (a lone Escape key press).
        let keep = (1..PREFIX.len())
            .rev()
            .find(|&n| rest.ends_with(&PREFIX[..n]))
            .unwrap_or(0);
        out.input.extend_from_slice(&rest[..rest.len() - keep]);
        self.held = rest[rest.len() - keep..].to_vec();
        out
    }

    /// Input held back as a possible packet start.
    pub fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Gives up on a held possible packet start: it was ordinary input.
    pub fn flush_held(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.held)
    }

    fn packet(&mut self, body: &[u8], out: &mut Filtered) {
        let body = String::from_utf8_lossy(body);
        let (meta, payload) = body.split_once(';').unwrap_or((&body, ""));
        let meta: Vec<(&str, &str)> = meta
            .split(':')
            .filter_map(|part| part.split_once('='))
            .collect();
        let get = |key: &str| meta.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        if get("type") != Some("read") {
            return;
        }
        match get("status") {
            Some("OK") => {
                if !matches!(self.state, State::Reading { .. }) {
                    self.state = State::Listing {
                        mimes: Vec::new(),
                        dot_payload: false,
                        pw: get("pw").map(str::to_owned),
                        loc: get("loc")
                            .filter(|loc| *loc == "primary")
                            .map(str::to_owned),
                    };
                }
            }
            Some("DATA") => {
                let Some(mime) = get("mime").and_then(decode_text) else {
                    return;
                };
                match &mut self.state {
                    State::Listing {
                        mimes, dot_payload, ..
                    } => {
                        if mime == LISTING {
                            if let Some(list) = decode_text(payload) {
                                *dot_payload = true;
                                mimes.extend(
                                    list.split_whitespace()
                                        .filter(|m| *m != LISTING)
                                        .map(str::to_owned),
                                );
                            }
                        } else {
                            mimes.push(mime);
                        }
                    }
                    State::Reading {
                        mime: wanted,
                        data,
                        too_big,
                        ..
                    } if *wanted == mime && !payload.is_empty() && !*too_big => {
                        match BASE64.decode(payload) {
                            Ok(chunk) if data.len() + chunk.len() <= self.limit => {
                                data.extend_from_slice(&chunk);
                            }
                            Ok(_) => *too_big = true,
                            Err(_) => {}
                        }
                    }
                    _ => {}
                }
            }
            Some("DONE") => match std::mem::replace(&mut self.state, State::Idle) {
                State::Listing {
                    mimes,
                    dot_payload,
                    pw,
                    loc,
                } => {
                    let chosen = IMAGE_TYPES
                        .iter()
                        .find(|image| mimes.iter().any(|m| m == *image))
                        .map(|image| (image.to_string(), true))
                        .or_else(|| {
                            mimes
                                .iter()
                                .any(|m| m == TEXT)
                                .then(|| (TEXT.to_owned(), false))
                        });
                    let Some((mime, image)) = chosen else {
                        out.errors
                            .push("Clipboard has no text or image to paste".into());
                        return;
                    };
                    let mut meta = vec!["type=read".to_owned()];
                    if let Some(loc) = loc {
                        meta.push(format!("loc={loc}"));
                    }
                    if let Some(pw) = pw {
                        meta.push(format!("pw={pw}"));
                        meta.push(format!("name={}", BASE64.encode("Paste event")));
                    }
                    let encoded = BASE64.encode(&mime);
                    let request = if dot_payload {
                        format!("\x1b]5522;{};{encoded}\x07", meta.join(":"))
                    } else {
                        meta.push(format!("mime={encoded}"));
                        format!("\x1b]5522;{}\x07", meta.join(":"))
                    };
                    out.replies.push(request.into_bytes());
                    self.state = State::Reading {
                        mime,
                        image,
                        data: Vec::new(),
                        too_big: false,
                    };
                }
                State::Reading {
                    mime,
                    image,
                    data,
                    too_big,
                } => {
                    if too_big {
                        out.errors.push("Pasted image is too large".into());
                    } else if data.is_empty() {
                        out.errors.push("Clipboard paste was empty".into());
                    } else if image {
                        out.images.push(PastedImage {
                            mime_type: mime,
                            bytes: data,
                        });
                    } else {
                        // Back into the input as the bracketed paste a plain paste would be.
                        out.input.extend_from_slice(b"\x1b[200~");
                        out.input.extend_from_slice(&data);
                        out.input.extend_from_slice(b"\x1b[201~");
                    }
                }
                State::Idle => {}
            },
            Some(status) => {
                self.state = State::Idle;
                out.errors.push(format!("Paste failed: {status}"));
            }
            None => {}
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The end of an OSC body: its index and the terminator's length (BEL or ST).
fn terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes.iter().enumerate().find_map(|(i, b)| match b {
        0x07 => Some((i, 1)),
        0x1b if bytes.get(i + 1) == Some(&b'\\') => Some((i, 2)),
        _ => None,
    })
}

fn decode_text(value: &str) -> Option<String> {
    String::from_utf8(BASE64.decode(value).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(meta: &str, payload: &str) -> Vec<u8> {
        format!("\x1b]5522;{meta};{payload}\x1b\\").into_bytes()
    }
    fn b64(text: impl AsRef<[u8]>) -> String {
        BASE64.encode(text)
    }

    /// Tern's per-type listing, then the data for the type acp-tsp asked for.
    fn paste(filter: &mut PasteFilter, mimes: &[&str], mime: &str, data: &[u8]) -> Vec<Filtered> {
        let mut steps = vec![filter.feed(&packet("type=read:status=OK:pw=secret", ""))];
        for m in mimes {
            steps.push(filter.feed(&packet(
                &format!("type=read:status=DATA:mime={}", b64(m)),
                "",
            )));
        }
        steps.push(filter.feed(&packet("type=read:status=DONE", "")));
        steps.push(filter.feed(&packet("type=read:status=OK", "")));
        for chunk in data.chunks(3) {
            steps.push(filter.feed(&packet(
                &format!("type=read:status=DATA:mime={}", b64(mime)),
                &b64(chunk),
            )));
        }
        steps.push(filter.feed(&packet("type=read:status=DONE", "")));
        steps
    }

    #[test]
    fn an_image_paste_asks_for_the_image_and_yields_its_bytes() {
        let mut filter = PasteFilter::new(1024);
        let steps = paste(
            &mut filter,
            &["text/plain", "image/png"],
            "image/png",
            b"PNGDATA",
        );
        let request = String::from_utf8(steps[3].replies[0].clone()).unwrap();
        assert_eq!(
            request,
            format!(
                "\x1b]5522;type=read:pw=secret:name={}:mime={}\x07",
                b64("Paste event"),
                b64("image/png")
            ),
            "the image wins over text, with the paste's password"
        );
        let last = steps.last().unwrap();
        assert_eq!(
            last.images,
            [PastedImage {
                mime_type: "image/png".into(),
                bytes: b"PNGDATA".to_vec()
            }]
        );
        assert!(
            steps.iter().all(|step| step.input.is_empty()),
            "packets never reach the keys"
        );
    }

    #[test]
    fn a_text_paste_becomes_a_bracketed_paste() {
        let mut filter = PasteFilter::new(1024);
        let steps = paste(
            &mut filter,
            &["text/plain"],
            "text/plain",
            "héllo ~/a b.png".as_bytes(),
        );
        assert_eq!(
            steps.last().unwrap().input,
            "\x1b[200~héllo ~/a b.png\x1b[201~".as_bytes()
        );
    }

    #[test]
    fn keys_around_and_packets_split_across_reads_survive() {
        let mut filter = PasteFilter::new(1024);
        let mut stream = b"ab".to_vec();
        stream.extend(packet("type=read:status=OK", ""));
        stream.extend(packet(
            &format!("type=read:status=DATA:mime={}", b64(".")),
            &b64("image/png text/plain"),
        ));
        stream.extend(packet("type=read:status=DONE", ""));
        stream.extend(b"cd\x1b");
        let mut input = Vec::new();
        let mut replies = Vec::new();
        // One byte at a time: every packet and the prefix itself are split.
        for byte in &stream {
            let out = filter.feed(std::slice::from_ref(byte));
            input.extend(out.input);
            replies.extend(out.replies);
        }
        assert!(filter.holding(), "a trailing Escape might start a packet");
        input.extend(filter.flush_held());
        assert_eq!(input, b"abcd\x1b", "and is ordinary input once flushed");
        // kitty's dot listing gets its request in the same form.
        assert_eq!(
            replies,
            [format!("\x1b]5522;type=read;{}\x07", b64("image/png")).into_bytes()]
        );
    }

    #[test]
    fn oversized_empty_and_refused_pastes_report_an_error() {
        let mut filter = PasteFilter::new(4);
        let steps = paste(&mut filter, &["image/png"], "image/png", b"too many bytes");
        assert!(steps.last().unwrap().images.is_empty());
        assert_eq!(steps.last().unwrap().errors, ["Pasted image is too large"]);

        let steps = paste(&mut filter, &["image/png"], "image/png", b"");
        assert_eq!(steps.last().unwrap().errors, ["Clipboard paste was empty"]);

        filter.feed(&packet("type=read:status=OK", ""));
        let refused = filter.feed(&packet("type=read:status=EPERM", ""));
        assert_eq!(refused.errors, ["Paste failed: EPERM"]);

        let steps = paste(&mut filter, &["application/pdf"], "", b"");
        assert_eq!(steps[2].errors, ["Clipboard has no text or image to paste"]);
    }
}
