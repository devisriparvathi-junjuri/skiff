//! Drops through `skiff attach --host`. A terminal types a dropped file's
//! path on this computer as a bracketed paste. The agent on the remote Host
//! cannot read that path, so each file goes to the Host's cache (`drops@1`,
//! as the app's drop does) and the paste gets the Host paths.

use std::{path::PathBuf, sync::Arc};

use skiff_client::host::{data, data::DataPlane, local::HostSelection, HostConnection};
use skiff_core::shell::quote_word;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Keys as they came, or the text of one whole bracketed paste.
#[derive(Debug, PartialEq)]
pub(crate) enum Piece {
    Keys(Vec<u8>),
    Paste(Vec<u8>),
}

/// Finds the bracketed pastes in the bytes read from stdin. A paste can
/// come over more than one read: its start waits here for its end.
#[derive(Default)]
pub(crate) struct Pastes {
    open: Option<Vec<u8>>,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

impl Pastes {
    pub(crate) fn feed(&mut self, mut data: &[u8]) -> Vec<Piece> {
        let mut out = Vec::new();
        loop {
            if let Some(open) = &mut self.open {
                open.extend_from_slice(data);
                let Some(end) = find(open, PASTE_END) else { return out };
                let rest = open.split_off(end + PASTE_END.len());
                open.truncate(end);
                out.push(Piece::Paste(self.open.take().unwrap()));
                if rest.is_empty() {
                    return out;
                }
                // Read again from the bytes after the paste.
                out.extend(self.feed(&rest));
                return out;
            }
            match find(data, PASTE_START) {
                None => {
                    if !data.is_empty() {
                        out.push(Piece::Keys(data.to_vec()));
                    }
                    return out;
                }
                Some(at) => {
                    if at > 0 {
                        out.push(Piece::Keys(data[..at].to_vec()));
                    }
                    self.open = Some(Vec::new());
                    data = &data[at + PASTE_START.len()..];
                }
            }
        }
    }
}

/// The paste's shell words. None: a quote is not closed.
fn words(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => out.extend(word.take()),
            '\\' => word.get_or_insert_default().push(chars.next()?),
            '\'' => {
                let w = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => w.push(c),
                            c => {
                                w.push('\\');
                                w.push(c);
                            }
                        },
                        c => w.push(c),
                    }
                }
            }
            c => word.get_or_insert_default().push(c),
        }
    }
    out.extend(word);
    Some(out)
}

/// A `file://` URI as a path. Percent escapes are decoded.
fn from_uri(word: &str) -> Option<PathBuf> {
    let rest = word.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// The pasted text as paths that exist on this computer: one or more
/// absolute paths or `file://` URIs, and nothing else. None: other text.
pub(crate) fn local_paths(text: &str) -> Option<Vec<PathBuf>> {
    let paths: Vec<PathBuf> = words(text)?
        .iter()
        .map(|w| from_uri(w).unwrap_or_else(|| PathBuf::from(w)))
        .collect();
    let ok = !paths.is_empty() && paths.iter().all(|p| p.is_absolute() && p.exists());
    ok.then_some(paths)
}

/// The remote Host a `skiff attach --host` runs on.
pub(crate) struct Remote {
    /// The Host's shown name, for messages.
    pub(crate) name: String,
    pub(crate) selection: HostSelection,
    /// The session plane, for the Host's identity.
    pub(crate) session: Arc<HostConnection>,
}

pub(crate) struct Drops {
    remote: Remote,
    data: Option<DataPlane>,
    pub(crate) pastes: Pastes,
}

impl Drops {
    pub(crate) fn new(remote: Remote) -> Self {
        Self { remote, data: None, pastes: Pastes::default() }
    }

    /// What to send for a paste: the same paste, or one with the Host paths
    /// of the local files it names. Err: the message to show; nothing is sent.
    pub(crate) async fn paste(&mut self, text: &[u8]) -> Result<Vec<u8>, String> {
        let same = || [PASTE_START, text, PASTE_END].concat();
        let Some(paths) = std::str::from_utf8(text).ok().and_then(local_paths) else { return Ok(same()) };
        let name = self.remote.name.clone();
        let on = Some(name.as_str());
        let mut files = Vec::new();
        for p in &paths {
            files.push(data::drop_file(p, on).await?);
        }
        let c = self.data().await?;
        let mut typed = Vec::new();
        for (name, bytes) in files {
            let path = data::drop_send(&c, &name, bytes, on).await?;
            typed.extend(format!("{} ", quote_word(&path)).into_bytes());
        }
        Ok([PASTE_START, &typed, PASTE_END].concat())
    }

    /// The Host's data plane, opened on the first drop.
    async fn data(&mut self) -> Result<Arc<HostConnection>, String> {
        if let Some(d) = &self.data {
            if !d.connection().is_closed() {
                return Ok(d.connection());
            }
        }
        let host = self.remote.session.hello().host.host_id.clone();
        let plane = self
            .remote
            .selection
            .connect_data(&host)
            .await
            .map_err(|e| format!("Skiff cannot reach skiff-host on {}: {e:#}", self.remote.name))?;
        let c = plane.connection();
        self.data = Some(plane);
        Ok(c)
    }
}

/// A path this computer shows, for a test.
#[cfg(test)]
fn shown(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_split_from_keys_over_reads() {
        let mut p = Pastes::default();
        assert_eq!(p.feed(b"ab"), vec![Piece::Keys(b"ab".to_vec())]);
        assert_eq!(p.feed(b"x\x1b[200~/tmp/a"), vec![Piece::Keys(b"x".to_vec())]);
        assert_eq!(p.feed(b".png\x1b[201~y"), vec![Piece::Paste(b"/tmp/a.png".to_vec()), Piece::Keys(b"y".to_vec())]);
        assert_eq!(
            p.feed(b"\x1b[200~1\x1b[201~\x1b[200~2\x1b[201~"),
            vec![Piece::Paste(b"1".to_vec()), Piece::Paste(b"2".to_vec())]
        );
    }

    #[test]
    fn only_existing_absolute_paths_are_files() {
        let dir = std::env::temp_dir().join(format!("skiff-drops-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a b.png");
        let b = dir.join("it's.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"y").unwrap();
        // Ghostty and iTerm escape with backslashes; others quote.
        let escaped = shown(&a).replace(' ', "\\ ");
        assert_eq!(local_paths(&escaped), Some(vec![a.clone()]));
        let both = format!("{} {}", quote_word(&shown(&a)), quote_word(&shown(&b)));
        assert_eq!(local_paths(&both), Some(vec![a.clone(), b.clone()]));
        assert_eq!(local_paths(&format!("\"{}\"", shown(&a))), Some(vec![a.clone()]));
        let uri = format!("file://{}", shown(&a).replace(' ', "%20"));
        assert_eq!(local_paths(&uri), Some(vec![a.clone()]));
        // Text, a missing file, a relative path, one file among words: unchanged.
        assert_eq!(local_paths("hello world"), None);
        assert_eq!(local_paths(""), None);
        assert_eq!(local_paths(&format!("{} /no/such/file", quote_word(&shown(&a)))), None);
        assert_eq!(local_paths("Cargo.toml"), None);
        assert_eq!(local_paths(&format!("see {}", quote_word(&shown(&a)))), None);
        assert_eq!(local_paths("'open"), None);
        // A folder counts here; the send refuses it.
        assert_eq!(local_paths(&shown(&dir)), Some(vec![dir.clone()]));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
