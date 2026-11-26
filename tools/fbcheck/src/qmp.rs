// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2025-2026 Kevin Barnard and minix.rs Contributors
//! Taking the dump: a QMP client just large enough for `stop`, `screendump`,
//! `cont` (slice 6.7, decision F11).
//!
//! QMP is line-oriented JSON over a socket — here the Unix socket
//! `tools/qemu-run.sh` gives every boot. The server speaks first, with a
//! greeting; the client must answer `qmp_capabilities` before any other
//! command; after that each command gets exactly one reply, an object with a
//! `return` member or an `error` member. Objects with an `event` member are
//! asynchronous and can arrive **between a command and its reply** — `stop`
//! is answered by a `STOP` event and then its `return` — so every wait for a
//! reply skips them.
//!
//! [`dump`] is the whole exchange. Two things about it are deliberate:
//!
//! * **The guest is stopped while it is dumped**, so a dump is one instant of
//!   the screen and not a tear across a scroll's repaint. (A dump can still
//!   catch a glyph half-painted; the check refuses it and the caller tries
//!   again.)
//! * **`cont` is always sent once `stop` has been**, whatever happened in
//!   between: a failed `screendump` must not leave a boot frozen for the rest
//!   of its timeout.
//!
//! QEMU resolves `screendump`'s `filename` against **its own** working
//! directory, not the caller's, so the path sent is always absolute.
//!
//! There is no JSON crate in this workspace (`deny.toml`), and none is
//! needed: the commands are four fixed strings and one with a file name in
//! it ([`screendump_command`]), and a reply only has to be told apart by its
//! top-level member names ([`classify`]). The scanner below reads exactly
//! that much JSON, strictly — a line it cannot follow is an error, never a
//! guess.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long one step of the exchange — the greeting, or one command's reply —
/// may take. A dump of a paused 800×600 screen is milliseconds; this only has
/// to keep a dead socket from hanging the caller.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Leaves capability-negotiation mode. Must be the first command.
pub const CAPABILITIES: &str = r#"{"execute":"qmp_capabilities"}"#;
/// Pauses the guest's CPUs.
pub const STOP: &str = r#"{"execute":"stop"}"#;
/// Resumes them.
pub const CONT: &str = r#"{"execute":"cont"}"#;

/// `s` as a JSON string, quotes included.
///
/// `"` and `\` are escaped, every control character below U+0020 is written
/// as `\u00XX` (JSON forbids them raw), and everything else — including
/// non-ASCII text — is passed through as the UTF-8 it already is.
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The `screendump` command writing to `filename`. No `format` argument: PPM
/// is the default, and has been since before the argument existed.
pub fn screendump_command(filename: &str) -> String {
    format!(
        r#"{{"execute":"screendump","arguments":{{"filename":{}}}}}"#,
        json_string(filename)
    )
}

/// What one line from the server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The banner sent on connection: it has a `QMP` member.
    Greeting,
    /// An asynchronous event, by name. Never a command's reply.
    Event(String),
    /// A command succeeded.
    Return,
    /// A command failed; the server's `desc`.
    Error(String),
}

/// Tell a line from the server apart by its top-level members. Text inside a
/// value — an error whose description contains the word `return`, say — never
/// decides it.
pub fn classify(line: &str) -> Result<Reply, String> {
    let members = members(line).map_err(|e| format!("not a QMP message ({e}): {line:?}"))?;
    let get = |name: &str| members.iter().find(|(key, _)| key == name).map(|(_, v)| *v);
    if get("QMP").is_some() {
        Ok(Reply::Greeting)
    } else if let Some(name) = get("event") {
        Ok(Reply::Event(string_value(name)))
    } else if let Some(error) = get("error") {
        let desc = self::members(error)
            .ok()
            .and_then(|m| m.into_iter().find(|(key, _)| key == "desc"))
            .map_or_else(|| error.to_string(), |(_, desc)| string_value(desc));
        Ok(Reply::Error(desc))
    } else if get("return").is_some() {
        Ok(Reply::Return)
    } else {
        Err(format!(
            "a QMP message with none of QMP, event, error, return: {line:?}"
        ))
    }
}

/// A raw JSON value as text: a string decoded, anything else as written.
fn string_value(raw: &str) -> String {
    let mut scan = Scan { text: raw, pos: 0 };
    scan.string().unwrap_or_else(|_| raw.to_string())
}

/// The members of a JSON object, in order: each name decoded, each value as
/// the raw text it was written in. `text` must be one object and nothing
/// else but white space.
fn members(text: &str) -> Result<Vec<(String, &str)>, String> {
    let mut scan = Scan { text, pos: 0 };
    let mut members = Vec::new();
    scan.space();
    scan.expect(b'{')?;
    scan.space();
    if scan.peek() == Some(b'}') {
        scan.pos += 1;
    } else {
        loop {
            scan.space();
            let key = scan.string()?;
            scan.space();
            scan.expect(b':')?;
            scan.space();
            members.push((key, scan.value()?));
            scan.space();
            match scan.peek() {
                Some(b',') => scan.pos += 1,
                Some(b'}') => {
                    scan.pos += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", scan.pos)),
            }
        }
    }
    scan.space();
    if scan.pos != text.len() {
        return Err(format!("text after the object at byte {}", scan.pos));
    }
    Ok(members)
}

/// A position in a piece of JSON.
struct Scan<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scan<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!(
                "expected '{}' at byte {}",
                char::from(byte),
                self.pos
            ))
        }
    }

    /// Decode the string starting at the cursor and move past its closing
    /// quote.
    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.char()?;
            match c {
                '"' => return Ok(out),
                '\\' => out.push(self.escape()?),
                c if u32::from(c) < 0x20 => {
                    return Err(format!(
                        "a raw control character in a string at byte {}",
                        self.pos
                    ));
                }
                c => out.push(c),
            }
        }
    }

    /// The next character, consumed.
    fn char(&mut self) -> Result<char, String> {
        let c = self
            .text
            .get(self.pos..)
            .and_then(|rest| rest.chars().next())
            .ok_or_else(|| "the text ends inside a string".to_string())?;
        self.pos += c.len_utf8();
        Ok(c)
    }

    /// What the escape after a backslash stands for.
    fn escape(&mut self) -> Result<char, String> {
        Ok(match self.char()? {
            '"' => '"',
            '\\' => '\\',
            '/' => '/',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'u' => {
                let high = self.hex4()?;
                // A surrogate pair is two escapes; anything unpaired becomes
                // the replacement character rather than an error — the text
                // is only ever shown to a person.
                let code = if (0xD800..0xDC00).contains(&high)
                    && self
                        .text
                        .get(self.pos..)
                        .is_some_and(|r| r.starts_with("\\u"))
                {
                    self.pos += 2;
                    let low = self.hex4()?;
                    if (0xDC00..0xE000).contains(&low) {
                        0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                    } else {
                        0xFFFD
                    }
                } else {
                    high
                };
                char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
            }
            other => return Err(format!("an unknown escape \\{other} at byte {}", self.pos)),
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("a \\u escape without four hex digits at byte {}", self.pos))?;
        self.pos += 4;
        u32::from_str_radix(digits, 16).map_err(|e| e.to_string())
    }

    /// Move past one value of any kind and return it as written.
    fn value(&mut self) -> Result<&'a str, String> {
        let start = self.pos;
        match self.peek() {
            Some(b'"') => {
                self.string()?;
            }
            Some(b'{' | b'[') => {
                // The closers still owed, innermost last.
                let mut open = Vec::new();
                loop {
                    match self.peek() {
                        Some(b'"') => {
                            self.string()?;
                            continue;
                        }
                        Some(b'{') => open.push(b'}'),
                        Some(b'[') => open.push(b']'),
                        Some(b @ (b'}' | b']')) => {
                            if open.pop() != Some(b) {
                                return Err(format!(
                                    "a stray '{}' at byte {}",
                                    char::from(b),
                                    self.pos
                                ));
                            }
                        }
                        Some(_) => {}
                        None => return Err("the text ends inside a value".to_string()),
                    }
                    self.pos += 1;
                    if open.is_empty() {
                        break;
                    }
                }
            }
            _ => {
                // A number, `true`, `false` or `null`: up to the next
                // delimiter.
                while self.peek().is_some_and(|b| {
                    !matches!(b, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                }) {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(format!("expected a value at byte {start}"));
                }
            }
        }
        self.text
            .get(start..self.pos)
            .ok_or_else(|| "a value ends inside a character".to_string())
    }
}

/// A connection that has been greeted.
pub struct Client {
    stream: BufReader<UnixStream>,
    timeout: Duration,
}

impl Client {
    /// Connect to the socket at `sock` and read the server's greeting. Every
    /// later step may take up to `timeout`.
    pub fn connect(sock: &Path, timeout: Duration) -> Result<Self, String> {
        let stream = UnixStream::connect(sock)
            .map_err(|e| format!("connecting to the QMP socket {}: {e}", sock.display()))?;
        // `set_*_timeout` refuses zero.
        let timeout = timeout.max(Duration::from_millis(1));
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| format!("QMP socket: {e}"))?;
        let mut client = Self {
            stream: BufReader::new(stream),
            timeout,
        };
        let deadline = Instant::now() + timeout;
        match client.read(deadline, "the greeting")? {
            Reply::Greeting => Ok(client),
            other => Err(format!("QMP: expected the greeting, got {other:?}")),
        }
    }

    /// Send one command and wait for its reply, skipping events. `name` is
    /// for the error message.
    pub fn execute(&mut self, name: &str, command: &str) -> Result<(), String> {
        let stream = self.stream.get_mut();
        stream
            .write_all(format!("{command}\n").as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("QMP {name}: sending the command: {e}"))?;
        let deadline = Instant::now() + self.timeout;
        let what = format!("the reply to {name}");
        loop {
            match self.read(deadline, &what)? {
                Reply::Event(_) => {}
                Reply::Return => return Ok(()),
                Reply::Error(desc) => return Err(format!("QMP {name} failed: {desc}")),
                Reply::Greeting => return Err(format!("QMP {name}: a second greeting")),
            }
        }
    }

    /// Read and classify one line, giving up at `deadline`.
    fn read(&mut self, deadline: Instant, what: &str) -> Result<Reply, String> {
        let mut line = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let timed_out =
                || format!("QMP: timed out after {:?} waiting for {what}", self.timeout);
            if left.is_zero() {
                return Err(timed_out());
            }
            self.stream
                .get_ref()
                .set_read_timeout(Some(left))
                .map_err(|e| format!("QMP: waiting for {what}: {e}"))?;
            match self.stream.read_until(b'\n', &mut line) {
                Ok(0) => return Err(format!("QMP: the socket closed while waiting for {what}")),
                Ok(_) if !line.ends_with(b"\n") => {
                    return Err(format!("QMP: the socket closed in the middle of {what}"));
                }
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    if text.trim().is_empty() {
                        line.clear();
                        continue;
                    }
                    return classify(text.trim());
                }
                // A timed-out read is retried against the deadline, which has
                // passed by now; whatever part of a line arrived stays in
                // `line` for the retry.
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("QMP: reading {what}: {e}")),
            }
        }
    }
}

/// Stop the guest, dump its screen to `out` as a PPM, and let it run again.
///
/// `out` is made absolute against this process's working directory first.
/// Once `stop` has been sent, `cont` is sent too, whatever the outcome; the
/// error returned is the first one that happened.
pub fn dump(sock: &Path, out: &Path, timeout: Duration) -> Result<(), String> {
    let absolute =
        std::path::absolute(out).map_err(|e| format!("making {} absolute: {e}", out.display()))?;
    let filename = absolute.to_str().ok_or_else(|| {
        format!(
            "{} is not UTF-8, so it cannot be named in a QMP command",
            absolute.display()
        )
    })?;

    let mut qmp = Client::connect(sock, timeout)?;
    qmp.execute("qmp_capabilities", CAPABILITIES)?;
    let stopped = qmp.execute("stop", STOP);
    let dumped = match stopped {
        Ok(()) => qmp.execute("screendump", &screendump_command(filename)),
        // Not stopped, as far as anyone knows: a dump now could tear.
        Err(_) => Ok(()),
    };
    let resumed = qmp
        .execute("cont", CONT)
        .map_err(|e| format!("{e} -- THE GUEST MAY STILL BE STOPPED"));
    stopped?;
    dumped?;
    resumed
}

/// A stand-in QMP server for the tests here and in `lib.rs`.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::JoinHandle;

    /// What QEMU 11 says on connection.
    pub const GREETING: &str = r#"{"QMP": {"version": {"qemu": {"micro": 3, "minor": 0, "major": 11}, "package": ""}, "capabilities": ["oob"]}}"#;
    pub const RETURN: &str = r#"{"return": {}}"#;
    pub const STOP_EVENT: &str =
        r#"{"timestamp": {"seconds": 1795000000, "microseconds": 12345}, "event": "STOP"}"#;
    pub const RESUME_EVENT: &str =
        r#"{"timestamp": {"seconds": 1795000001, "microseconds": 54321}, "event": "RESUME"}"#;

    /// A directory of the test's own, removed when dropped.
    ///
    /// Its path is kept SHORT: a Unix socket's path is limited to 104 bytes
    /// on macOS (108 on Linux), and macOS's per-user temporary directory is
    /// half of that before a file name is added. `/tmp` is used when the
    /// system's choice is long.
    pub struct Scratch(PathBuf);

    impl Scratch {
        pub fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let base = std::env::temp_dir();
            let base = if base.as_os_str().len() <= 40 {
                base
            } else {
                PathBuf::from("/tmp")
            };
            let dir = base.join(format!(
                "fbck-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The command name in a line the client sent.
    pub fn executed(command: &str) -> String {
        let members = members(command).unwrap();
        let (_, name) = members.iter().find(|(key, _)| key == "execute").unwrap();
        string_value(name)
    }

    /// The `filename` argument of a `screendump` the client sent.
    pub fn filename(command: &str) -> String {
        let outer = members(command).unwrap();
        let (_, arguments) = outer.iter().find(|(key, _)| key == "arguments").unwrap();
        let inner = members(arguments).unwrap();
        let (_, name) = inner.iter().find(|(key, _)| key == "filename").unwrap();
        string_value(name)
    }

    /// Serve ONE connection on `sock`: send `greeting` if there is one, then
    /// answer each line the client sends with the lines `respond` gives for
    /// it, until the client hangs up. The thread returns every line it was
    /// sent.
    pub fn serve(
        sock: &Path,
        greeting: Option<&'static str>,
        respond: impl Fn(&str) -> Vec<String> + Send + 'static,
    ) -> JoinHandle<Vec<String>> {
        let listener = UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut writer = stream.try_clone().unwrap();
            if let Some(greeting) = greeting {
                writeln!(writer, "{greeting}").unwrap();
            }
            let mut seen = Vec::new();
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                for reply in respond(&line) {
                    // The client may have gone; that is its business.
                    let _ = writeln!(writer, "{reply}");
                }
                seen.push(line);
            }
            seen
        })
    }

    /// A healthy QEMU: events around `stop` and `cont`, and a `screendump`
    /// that writes `image` to the file it is told to.
    pub fn healthy(image: Vec<u8>) -> impl Fn(&str) -> Vec<String> + Send + 'static {
        move |command| match executed(command).as_str() {
            "stop" => vec![STOP_EVENT.into(), RETURN.into()],
            "cont" => vec![RESUME_EVENT.into(), RETURN.into()],
            "screendump" => {
                std::fs::write(filename(command), &image).unwrap();
                vec![RETURN.into()]
            }
            _ => vec![RETURN.into()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    const SHORT: Duration = Duration::from_millis(300);

    fn names(commands: &[String]) -> Vec<String> {
        commands.iter().map(|c| executed(c)).collect()
    }

    // --- the command builder -------------------------------------------------

    #[test]
    fn json_string_escapes_what_json_requires() {
        assert_eq!(json_string("plain"), r#""plain""#);
        assert_eq!(json_string(""), r#""""#);
        assert_eq!(json_string(r#"a"b"#), r#""a\"b""#);
        assert_eq!(json_string(r"a\b"), r#""a\\b""#);
        assert_eq!(json_string(r#"\""#), r#""\\\"""#);
        assert_eq!(json_string("a\nb\tc\rd"), r#""a\u000ab\u0009c\u000dd""#);
        assert_eq!(json_string("\u{0}\u{1}\u{1f}"), r#""\u0000\u0001\u001f""#);
        // U+0020 and up pass through, DEL and non-ASCII included.
        assert_eq!(json_string(" ~\u{7f}"), "\" ~\u{7f}\"");
        assert_eq!(json_string("dé/日本 🙂"), "\"dé/日本 🙂\"");
    }

    #[test]
    fn every_string_survives_being_written_and_read_back() {
        let mut all_controls = String::new();
        for c in 0..0x20u8 {
            all_controls.push(char::from(c));
        }
        for original in [
            "/Users/someone/src/minixrs/target/screen.ppm",
            r#"/tmp/a "quoted" name.ppm"#,
            r"C:\not\a\unix\path",
            r#"\"\\"\"#,
            "tab\there, newline\nthere",
            "dé/日本/🙂.ppm",
            "",
            all_controls.as_str(),
        ] {
            let written = json_string(original);
            assert!(
                written.bytes().all(|b| b >= 0x20),
                "a raw control character in {written:?}"
            );
            let mut scan = Scan {
                text: &written,
                pos: 0,
            };
            assert_eq!(scan.string().as_deref(), Ok(original));
            assert_eq!(scan.pos, written.len(), "the whole string was one string");
        }
    }

    #[test]
    fn the_fixed_commands_are_what_qemu_documents() {
        assert_eq!(CAPABILITIES, r#"{"execute":"qmp_capabilities"}"#);
        assert_eq!(STOP, r#"{"execute":"stop"}"#);
        assert_eq!(CONT, r#"{"execute":"cont"}"#);
        for command in [CAPABILITIES, STOP, CONT] {
            assert!(!command.contains('\n'), "one command, one line");
            assert_eq!(members(command).unwrap().len(), 1);
        }
    }

    #[test]
    fn the_screendump_command_names_the_file() {
        assert_eq!(
            screendump_command("/tmp/screen.ppm"),
            r#"{"execute":"screendump","arguments":{"filename":"/tmp/screen.ppm"}}"#
        );
        // A name that would break out of its string if it were not escaped.
        let nasty = "/tmp/a\"},\"execute\":\"quit\",\"x\":{\"y\":\"\\\n.ppm";
        let command = screendump_command(nasty);
        assert!(!command.contains('\n'), "still one line: {command}");
        assert_eq!(executed(&command), "screendump");
        assert_eq!(filename(&command), nasty);
        let outer = members(&command).unwrap();
        assert_eq!(
            outer.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["execute", "arguments"]
        );
    }

    // --- the reply classifier ------------------------------------------------

    #[test]
    fn the_four_kinds_of_line_are_told_apart() {
        assert_eq!(classify(GREETING), Ok(Reply::Greeting));
        assert_eq!(classify(RETURN), Ok(Reply::Return));
        assert_eq!(classify(r#"{"return":{}}"#), Ok(Reply::Return));
        assert_eq!(classify(STOP_EVENT), Ok(Reply::Event("STOP".into())));
        assert_eq!(classify(RESUME_EVENT), Ok(Reply::Event("RESUME".into())));
        assert_eq!(
            classify(
                r#"{"error": {"class": "GenericError", "desc": "Could not open '/nope/x.ppm': No such file or directory"}}"#
            ),
            Ok(Reply::Error(
                "Could not open '/nope/x.ppm': No such file or directory".into()
            ))
        );
        // Surrounding white space, and a reply carrying an id.
        assert_eq!(
            classify("  {\"return\": {}, \"id\": 7}\r\n"),
            Ok(Reply::Return)
        );
        // Values that are not objects.
        assert_eq!(
            classify(r#"{"return": [1, 2, {"a": "]"}]}"#),
            Ok(Reply::Return)
        );
        assert_eq!(classify(r#"{"return": true}"#), Ok(Reply::Return));
        assert_eq!(classify(r#"{"return": -1.5e3}"#), Ok(Reply::Return));
    }

    #[test]
    fn only_top_level_member_names_decide() {
        // An error that talks about a return, an event and a greeting.
        assert_eq!(
            classify(
                r#"{"error": {"class": "X", "desc": "no \"return\" for {\"event\": 1} QMP"}}"#
            ),
            Ok(Reply::Error("no \"return\" for {\"event\": 1} QMP".into()))
        );
        // A return whose value has members called error and event.
        assert_eq!(
            classify(r#"{"return": {"error": {"desc": "x"}, "event": "STOP", "QMP": 1}}"#),
            Ok(Reply::Return)
        );
        // An event whose data mentions an error.
        assert_eq!(
            classify(r#"{"event": "BLOCK_IO_ERROR", "data": {"error": "eio", "return": 0}}"#),
            Ok(Reply::Event("BLOCK_IO_ERROR".into()))
        );
        // A string value that looks like the end of the object.
        assert_eq!(
            classify(r#"{"return": "}, \"error\": {\"desc\": \"no\"}"}"#),
            Ok(Reply::Return)
        );
    }

    #[test]
    fn an_error_without_a_description_is_still_an_error() {
        assert_eq!(
            classify(r#"{"error": {"class": "GenericError"}}"#),
            Ok(Reply::Error(r#"{"class": "GenericError"}"#.into()))
        );
        assert_eq!(
            classify(r#"{"error": "bare"}"#),
            Ok(Reply::Error(r#""bare""#.into()))
        );
    }

    #[test]
    fn a_line_that_is_not_one_qmp_object_is_refused() {
        for line in [
            "",
            "   ",
            "garbage",
            "[]",
            r#""return""#,
            "{}",
            r#"{"id": 1}"#,
            r#"{"return": {}"#,
            r#"{"return": {}} trailing"#,
            r#"{"return": {}}{"return": {}}"#,
            r#"{"return": }"#,
            r#"{"return" {}}"#,
            r#"{return: {}}"#,
            r#"{"return": {]}"#,
            r#"{"return": "unterminated}"#,
            r#"{"return": "bad \q escape"}"#,
            r#"{"return": "bad \u12 escape"}"#,
            "{\"return\": \"raw\ncontrol\"}",
            r#"{"return": {}},"#,
        ] {
            assert!(classify(line).is_err(), "{line:?} was accepted");
        }
    }

    #[test]
    fn string_escapes_decode() {
        let mut scan = Scan {
            text: r#""\" \\ \/ \b \f \n \r \t \u0041 \u00e9 \ud83d\ude42 \ud83d x""#,
            pos: 0,
        };
        assert_eq!(
            scan.string().unwrap(),
            "\" \\ / \u{8} \u{c} \n \r \t A é 🙂 \u{fffd} x"
        );
    }

    // --- the exchange, against a stand-in server ---------------------------------

    #[test]
    fn dump_stops_dumps_and_continues_in_that_order() {
        let scratch = Scratch::new();
        let (sock, out) = (scratch.path("q.sock"), scratch.path("screen.ppm"));
        let server = serve(&sock, Some(GREETING), healthy(b"the image".to_vec()));

        dump(&sock, &out, SHORT).unwrap();

        let commands = server.join().unwrap();
        assert_eq!(
            names(&commands),
            ["qmp_capabilities", "stop", "screendump", "cont"]
        );
        assert_eq!(filename(&commands[2]), out.to_str().unwrap());
        assert_eq!(std::fs::read(&out).unwrap(), b"the image");
    }

    #[test]
    fn a_relative_output_path_is_sent_absolute() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        // The server only records: nothing is written anywhere.
        let server = serve(&sock, Some(GREETING), |_| vec![RETURN.into()]);

        dump(&sock, Path::new("some/relative dir/screen.ppm"), SHORT).unwrap();

        let commands = server.join().unwrap();
        let sent = filename(&commands[2]);
        assert!(Path::new(&sent).is_absolute(), "{sent}");
        assert_eq!(
            Path::new(&sent),
            std::env::current_dir()
                .unwrap()
                .join("some/relative dir/screen.ppm")
        );
    }

    #[test]
    fn cont_is_sent_even_when_screendump_fails() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some(GREETING), |command| {
            match executed(command).as_str() {
                "stop" => vec![STOP_EVENT.into(), RETURN.into()],
                "screendump" => vec![
                    r#"{"error": {"class": "GenericError", "desc": "no such console"}}"#.into(),
                ],
                _ => vec![RETURN.into()],
            }
        });

        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(
            err.contains("screendump") && err.contains("no such console"),
            "{err}"
        );

        let commands = server.join().unwrap();
        assert_eq!(
            names(&commands),
            ["qmp_capabilities", "stop", "screendump", "cont"]
        );
    }

    #[test]
    fn cont_is_sent_and_no_dump_taken_when_stop_fails() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some(GREETING), |command| {
            match executed(command).as_str() {
                "stop" => {
                    vec![r#"{"error": {"class": "GenericError", "desc": "cannot stop"}}"#.into()]
                }
                _ => vec![RETURN.into()],
            }
        });

        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("stop") && err.contains("cannot stop"), "{err}");
        assert_eq!(
            names(&server.join().unwrap()),
            ["qmp_capabilities", "stop", "cont"]
        );
    }

    #[test]
    fn a_failed_cont_says_the_guest_may_be_stopped() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some(GREETING), |command| {
            match executed(command).as_str() {
                "cont" => vec![r#"{"error": {"class": "GenericError", "desc": "nope"}}"#.into()],
                _ => vec![RETURN.into()],
            }
        });
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("cont") && err.contains("STOPPED"), "{err}");
        server.join().unwrap();
    }

    #[test]
    fn a_refused_capabilities_command_stops_before_stop() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some(GREETING), |_| {
            vec![r#"{"error": {"class": "CommandNotFound", "desc": "negotiate first"}}"#.into()]
        });
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("qmp_capabilities"), "{err}");
        // The guest was never stopped, so there is nothing to continue.
        assert_eq!(names(&server.join().unwrap()), ["qmp_capabilities"]);
    }

    #[test]
    fn events_between_a_command_and_its_reply_are_skipped() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some(GREETING), |_| {
            vec![
                STOP_EVENT.into(),
                String::new(),
                RESUME_EVENT.into(),
                r#"{"event": "RTC_CHANGE", "data": {"offset": 0}}"#.into(),
                RETURN.into(),
            ]
        });
        let mut qmp = Client::connect(&sock, SHORT).unwrap();
        qmp.execute("stop", STOP).unwrap();
        qmp.execute("cont", CONT).unwrap();
        drop(qmp);
        assert_eq!(names(&server.join().unwrap()), ["stop", "cont"]);
    }

    #[test]
    fn a_server_that_never_greets_times_out() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, None, |_| vec![]);
        let started = Instant::now();
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(
            err.contains("timed out") && err.contains("greeting"),
            "{err}"
        );
        assert!(started.elapsed() >= SHORT, "it waited the timeout out");
        assert!(started.elapsed() < Duration::from_secs(5), "and no longer");
        assert!(server.join().unwrap().is_empty(), "nothing was sent");
    }

    #[test]
    fn a_server_that_stops_answering_times_out_and_cont_is_still_sent() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        // Answers the capabilities command and then nothing, but only ever
        // sends events for `stop`: a reply that never comes.
        let server = serve(&sock, Some(GREETING), |command| {
            match executed(command).as_str() {
                "qmp_capabilities" => vec![RETURN.into()],
                "stop" => vec![STOP_EVENT.into()],
                _ => vec![],
            }
        });
        let started = Instant::now();
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("timed out") && err.contains("stop"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            names(&server.join().unwrap()),
            ["qmp_capabilities", "stop", "cont"]
        );
    }

    #[test]
    fn a_socket_that_closes_mid_exchange_is_an_error_not_a_hang() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        // Greets, then hangs up without reading anything.
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            writeln!(stream, "{GREETING}").unwrap();
        });
        // Which step notices depends on who wins the race: the write, the
        // read after it, or (macOS) setting a timeout on a socket whose peer
        // has gone. Each is an error, at once.
        let started = Instant::now();
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("QMP"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
        server.join().unwrap();
    }

    #[test]
    fn a_missing_socket_is_an_error() {
        let scratch = Scratch::new();
        let err = dump(&scratch.path("nobody.sock"), &scratch.path("s.ppm"), SHORT).unwrap_err();
        assert!(
            err.contains("connecting") && err.contains("nobody.sock"),
            "{err}"
        );
    }

    #[test]
    fn something_that_is_not_qmp_on_the_socket_is_refused() {
        let scratch = Scratch::new();
        let sock = scratch.path("q.sock");
        let server = serve(&sock, Some("SSH-2.0-OpenSSH_9.9"), |_| vec![]);
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("not a QMP message"), "{err}");
        server.join().unwrap();

        // JSON, but a reply where the greeting should be.
        let sock = scratch.path("r.sock");
        let server = serve(&sock, Some(RETURN), |_| vec![]);
        let err = dump(&sock, &scratch.path("screen.ppm"), SHORT).unwrap_err();
        assert!(err.contains("expected the greeting"), "{err}");
        server.join().unwrap();
    }
}
