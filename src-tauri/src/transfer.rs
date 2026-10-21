//! The resumable chunked upload state machine, independent of HTTP so it can be
//! tested against a fake server. `net::Api` is the real transport.

use std::future::Future;
use std::io::SeekFrom;
use std::sync::Arc;
use std::time::Duration;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::model::{ErrorKind, UploadError};

/// Bytes per PATCH request.
pub const CHUNK: u64 = 8 * 1024 * 1024;
/// Bytes per body piece; progress is reported as each piece is handed to the connection.
pub const PIECE: usize = 256 * 1024;
pub const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

pub struct Policy {
    pub chunk: u64,
    /// One entry per allowed retry.
    pub backoff: &'static [Duration],
}

pub const POLICY: Policy = Policy { chunk: CHUNK, backoff: &BACKOFF };

/// Called with the number of bytes handed to the connection so far.
pub type OnSent = Arc<dyn Fn(u64) + Send + Sync>;

/// A server reply to `PATCH /api/files/{name}`.
#[derive(Debug)]
pub enum Reply {
    /// 201 with the public URL.
    Created(String),
    /// 202 with the new offset.
    Accepted(u64),
    /// 409 `offset` with the server's offset.
    Offset(u64),
    /// 409 `exists`.
    Exists,
    /// 409 `busy`.
    Busy,
    Status(u16),
}

/// A server reply to `HEAD /api/files/{name}`.
#[derive(Debug)]
pub enum Head {
    Partial(u64),
    Complete(u64),
    Missing,
    Status(u16),
}

/// A connection failure or timeout.
#[derive(Debug)]
pub struct NetError;

pub trait Transport: Sync {
    fn patch(
        &self,
        name: &str,
        offset: u64,
        total: u64,
        body: Vec<Vec<u8>>,
        on_sent: OnSent,
    ) -> impl Future<Output = Result<Reply, NetError>> + Send;
    fn head(&self, name: &str) -> impl Future<Output = Result<Head, NetError>> + Send;
}

#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// The server's URL from the 201 response.
    Url(String),
    /// The file was already complete on the server (an earlier 201 was lost).
    Exists,
}

pub fn auth_error() -> UploadError {
    UploadError::new(ErrorKind::Auth, "The token wasn’t accepted.")
}

pub fn network_error() -> UploadError {
    UploadError::new(ErrorKind::Network, "Can’t reach the server.")
}

fn server_error(message: impl Into<String>) -> UploadError {
    UploadError::new(ErrorKind::Server, message)
}

fn unexpected() -> UploadError {
    server_error("The server sent an unexpected reply.")
}

enum Resume {
    At(u64),
    Done,
}

/// Asks the server how much of `name` it has. `None` means "unknown, keep going".
async fn resume_point<T: Transport>(t: &T, name: &str, total: u64) -> Result<Option<Resume>, UploadError> {
    match t.head(name).await {
        Ok(Head::Partial(offset)) if offset <= total => Ok(Some(Resume::At(offset))),
        Ok(Head::Missing) => Ok(Some(Resume::At(0))),
        Ok(Head::Complete(len)) if len == total => Ok(Some(Resume::Done)),
        Ok(Head::Status(401)) => Err(auth_error()),
        Ok(Head::Status(s)) if s >= 500 => Ok(None),
        Ok(_) => Err(unexpected()),
        Err(NetError) => Ok(None),
    }
}

async fn read_pieces(file: &mut File, offset: u64, len: u64) -> std::io::Result<Vec<Vec<u8>>> {
    file.seek(SeekFrom::Start(offset)).await?;
    let mut pieces = Vec::new();
    let mut left = len;
    while left > 0 {
        let mut piece = vec![0; left.min(PIECE as u64) as usize];
        file.read_exact(&mut piece).await?;
        left -= piece.len() as u64;
        pieces.push(piece);
    }
    Ok(pieces)
}

/// Uploads `file` (`total` bytes) as `name`, chunk by chunk. With `resume`, first asks the
/// server where to continue. Network errors, timeouts, 5xx and `busy` are retried with
/// backoff, re-syncing the offset with `HEAD` before each retry. The retry budget starts
/// over whenever the server's copy grows, so a slow link that keeps making progress never
/// runs out of retries.
pub async fn send<T: Transport>(
    t: &T,
    file: &mut File,
    name: &str,
    total: u64,
    resume: bool,
    policy: &Policy,
    progress: &OnSent,
) -> Result<Sent, UploadError> {
    let mut offset = 0;
    if resume {
        match resume_point(t, name, total).await? {
            Some(Resume::At(at)) => offset = at,
            Some(Resume::Done) => return Ok(Sent::Exists),
            None => {}
        }
    }
    let mut retries = 0;
    loop {
        let start = offset;
        let len = policy.chunk.min(total - offset);
        let body = read_pieces(file, offset, len)
            .await
            .map_err(|_| UploadError::new(ErrorKind::Missing, "The file changed while uploading."))?;
        let on_sent: OnSent = {
            let (progress, base) = (progress.clone(), offset);
            Arc::new(move |n| progress(base + n))
        };
        let failure = match t.patch(name, offset, total, body, on_sent).await {
            Ok(Reply::Created(url)) => return Ok(Sent::Url(url)),
            Ok(Reply::Accepted(at)) if at > offset && at <= total => {
                offset = at;
                retries = 0;
                continue;
            }
            Ok(Reply::Offset(at)) if at != offset && at <= total => {
                offset = at;
                continue;
            }
            Ok(Reply::Accepted(_) | Reply::Offset(_)) => return Err(unexpected()),
            // Our own earlier attempt finished but its reply was lost, or (astronomically
            // unlikely) the random name is taken: only the first counts as success. If the
            // server can't say which right now, ask again after the usual backoff.
            Ok(Reply::Exists) => match resume_point(t, name, total).await? {
                Some(Resume::Done) => return Ok(Sent::Exists),
                Some(Resume::At(_)) => return Err(server_error("A file with this name already exists.")),
                None => network_error(),
            },
            Ok(Reply::Status(401)) => return Err(auth_error()),
            Ok(Reply::Status(413)) => return Err(server_error("The file is too large for the server.")),
            Ok(Reply::Busy) => server_error("The server is busy. Try again in a moment."),
            Ok(Reply::Status(s)) if s >= 500 => server_error("The server ran into a problem."),
            Ok(Reply::Status(s)) => return Err(server_error(format!("The server refused the upload ({s})."))),
            Err(NetError) => network_error(),
        };
        let Some(delay) = policy.backoff.get(retries) else { return Err(failure) };
        retries += 1;
        tokio::time::sleep(*delay).await;
        match resume_point(t, name, total).await? {
            Some(Resume::At(at)) => {
                if at > start {
                    retries = 0;
                }
                offset = at;
            }
            Some(Resume::Done) => return Ok(Sent::Exists),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// What the fake does to one PATCH instead of following the server rules.
    #[derive(Clone, Copy)]
    enum Fault {
        /// Stores `n` bytes of the body, then the connection drops.
        DropAfter(u64),
        Status(u16),
        Busy,
    }

    /// An in-memory server following the server's PATCH/HEAD rules, with scripted faults.
    struct Fake {
        data: Vec<u8>,
        stored: Mutex<Vec<u8>>,
        complete: Mutex<bool>,
        faults: Mutex<VecDeque<Option<Fault>>>,
        head_faults: Mutex<VecDeque<Head>>,
        log: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new(data: &[u8], faults: Vec<Option<Fault>>) -> Self {
            Self {
                data: data.to_vec(),
                stored: Mutex::new(Vec::new()),
                complete: Mutex::new(false),
                faults: Mutex::new(faults.into()),
                head_faults: Mutex::new(VecDeque::new()),
                log: Mutex::new(Vec::new()),
            }
        }
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Transport for Fake {
        async fn patch(&self, name: &str, offset: u64, total: u64, body: Vec<Vec<u8>>, on_sent: OnSent) -> Result<Reply, NetError> {
            let body: Vec<u8> = body.concat();
            assert_eq!(total, self.data.len() as u64);
            assert_eq!(body, self.data[offset as usize..offset as usize + body.len()], "body matches the file");
            self.log.lock().unwrap().push(format!("PATCH {offset}+{}", body.len()));
            on_sent(body.len() as u64);
            let fault = self.faults.lock().unwrap().pop_front().flatten();
            let mut stored = self.stored.lock().unwrap();
            match fault {
                Some(Fault::Status(s)) => return Ok(Reply::Status(s)),
                Some(Fault::Busy) => return Ok(Reply::Busy),
                _ => {}
            }
            if *self.complete.lock().unwrap() {
                return Ok(Reply::Exists);
            }
            if offset != stored.len() as u64 {
                return Ok(Reply::Offset(stored.len() as u64));
            }
            if let Some(Fault::DropAfter(n)) = fault {
                stored.extend_from_slice(&body[..n as usize]);
                return Err(NetError);
            }
            stored.extend_from_slice(&body);
            if stored.len() as u64 == total {
                assert_eq!(*stored, self.data);
                *self.complete.lock().unwrap() = true;
                Ok(Reply::Created(format!("https://example.com/i/{name}")))
            } else {
                Ok(Reply::Accepted(stored.len() as u64))
            }
        }

        async fn head(&self, _name: &str) -> Result<Head, NetError> {
            self.log.lock().unwrap().push("HEAD".into());
            if let Some(h) = self.head_faults.lock().unwrap().pop_front() {
                return Ok(h);
            }
            let stored = self.stored.lock().unwrap().len() as u64;
            Ok(if *self.complete.lock().unwrap() {
                Head::Complete(stored)
            } else if stored == 0 {
                Head::Missing
            } else {
                Head::Partial(stored)
            })
        }
    }

    const ZERO: [Duration; 5] = [Duration::ZERO; 5];
    const TEST: Policy = Policy { chunk: 10, backoff: &ZERO };

    fn temp_file(data: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!("push-transfer-{}-{}", std::process::id(), crate::model::new_id()));
        std::fs::write(&path, data).unwrap();
        path
    }

    /// Runs `send` against `fake` and returns the result plus every progress value reported.
    fn run(fake: &Fake, resume: bool) -> (Result<Sent, UploadError>, Vec<u64>) {
        let path = temp_file(&fake.data);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let progress: OnSent = {
            let seen = seen.clone();
            Arc::new(move |n| seen.lock().unwrap().push(n))
        };
        let result = tauri::async_runtime::block_on(async {
            let mut file = File::open(&path).await.unwrap();
            send(fake, &mut file, "abc.png", fake.data.len() as u64, resume, &TEST, &progress).await
        });
        std::fs::remove_file(path).unwrap();
        let seen = seen.lock().unwrap().clone();
        (result, seen)
    }

    const DATA: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxy"; // 35 bytes: chunks of 10, 10, 10, 5

    #[test]
    fn uploads_in_chunks() {
        let fake = Fake::new(DATA, vec![]);
        let (result, progress) = run(&fake, false);
        assert_eq!(result, Ok(Sent::Url("https://example.com/i/abc.png".into())));
        assert_eq!(fake.log(), ["PATCH 0+10", "PATCH 10+10", "PATCH 20+10", "PATCH 30+5"]);
        assert_eq!(progress, [10, 20, 30, 35]);
    }

    #[test]
    fn network_error_resumes_from_the_server_offset() {
        let fake = Fake::new(DATA, vec![None, Some(Fault::DropAfter(4))]);
        let (result, _) = run(&fake, false);
        assert!(matches!(result, Ok(Sent::Url(_))));
        assert_eq!(
            fake.log(),
            ["PATCH 0+10", "PATCH 10+10", "HEAD", "PATCH 14+10", "PATCH 24+10", "PATCH 34+1"]
        );
    }

    #[test]
    fn offset_conflict_resumes_without_a_retry() {
        // The client thinks it starts at 0, the server already has 10 bytes.
        let fake = Fake::new(DATA, vec![]);
        fake.stored.lock().unwrap().extend_from_slice(&DATA[..10]);
        let (result, _) = run(&fake, false);
        assert!(matches!(result, Ok(Sent::Url(_))));
        assert_eq!(fake.log(), ["PATCH 0+10", "PATCH 10+10", "PATCH 20+10", "PATCH 30+5"]);
    }

    #[test]
    fn resume_starts_from_head() {
        let fake = Fake::new(DATA, vec![]);
        fake.stored.lock().unwrap().extend_from_slice(&DATA[..25]);
        let (result, _) = run(&fake, true);
        assert!(matches!(result, Ok(Sent::Url(_))));
        assert_eq!(fake.log(), ["HEAD", "PATCH 25+10"]);
    }

    #[test]
    fn unauthorized_fails_immediately() {
        let fake = Fake::new(DATA, vec![Some(Fault::Status(401))]);
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Auth);
        assert_eq!(fake.log(), ["PATCH 0+10"]);
    }

    #[test]
    fn client_errors_fail_without_retry() {
        let fake = Fake::new(DATA, vec![Some(Fault::Status(413))]);
        let (result, _) = run(&fake, false);
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Server);
        assert_eq!(error.message, "The file is too large for the server.");
        assert_eq!(fake.log(), ["PATCH 0+10"]);
    }

    #[test]
    fn server_errors_retry_five_times_then_fail() {
        let fake = Fake::new(DATA, vec![Some(Fault::Status(502)); 6]);
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Server);
        let log = fake.log();
        assert_eq!(log.iter().filter(|l| l.starts_with("PATCH")).count(), 6);
        assert_eq!(log.iter().filter(|l| *l == "HEAD").count(), 5);
    }

    #[test]
    fn retries_reset_after_progress() {
        // Five failures, progress, five more failures: still succeeds.
        let mut faults = vec![Some(Fault::Status(503)); 5];
        faults.push(None);
        faults.extend([Some(Fault::Busy); 5]);
        let fake = Fake::new(DATA, faults);
        let (result, _) = run(&fake, false);
        assert!(matches!(result, Ok(Sent::Url(_))));
    }

    #[test]
    fn retries_reset_when_head_shows_progress() {
        // A slow link: every PATCH times out, but each one lands 4 more bytes. More failures
        // in a row than the retry budget, yet it keeps going because the offset moves.
        let fake = Fake::new(DATA, vec![Some(Fault::DropAfter(4)); 8]);
        let (result, _) = run(&fake, false);
        assert!(matches!(result, Ok(Sent::Url(_))));
        let log = fake.log();
        assert_eq!(log.iter().filter(|l| *l == "HEAD").count(), 8);
        assert_eq!(log.last().unwrap(), "PATCH 32+3");
    }

    #[test]
    fn network_errors_exhaust_as_network() {
        let fake = Fake::new(DATA, vec![Some(Fault::DropAfter(0)); 6]);
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Network);
    }

    #[test]
    fn lost_created_reply_counts_as_done() {
        // The last chunk lands but the connection drops before the 201 arrives.
        let fake = Fake::new(DATA, vec![None, None, None, Some(Fault::DropAfter(5))]);
        // Completing happens inside the fake only on a clean reply, so emulate it via HEAD.
        fake.head_faults.lock().unwrap().push_back(Head::Complete(DATA.len() as u64));
        let (result, _) = run(&fake, false);
        assert_eq!(result, Ok(Sent::Exists));
        assert_eq!(fake.log().last().unwrap(), "HEAD");
    }

    #[test]
    fn exists_needs_a_matching_length() {
        let fake = Fake::new(DATA, vec![]);
        *fake.complete.lock().unwrap() = true;
        fake.stored.lock().unwrap().extend_from_slice(DATA);
        let (result, _) = run(&fake, false);
        assert_eq!(result, Ok(Sent::Exists));

        let fake = Fake::new(DATA, vec![]);
        *fake.complete.lock().unwrap() = true;
        fake.stored.lock().unwrap().extend_from_slice(&DATA[..3]);
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Server);
    }

    #[test]
    fn exists_with_an_unknown_head_is_retried() {
        // The 201 was lost; the next PATCH says "exists" but the HEAD after it hits a 502.
        let fake = Fake::new(DATA, vec![]);
        *fake.complete.lock().unwrap() = true;
        fake.stored.lock().unwrap().extend_from_slice(DATA);
        fake.head_faults.lock().unwrap().push_back(Head::Status(502));
        let (result, _) = run(&fake, false);
        assert_eq!(result, Ok(Sent::Exists));
        assert_eq!(fake.log(), ["PATCH 0+10", "HEAD", "HEAD"]);

        // Still unknown after every retry (6 PATCHes, each followed by a HEAD, plus a HEAD
        // before each of the 5 retries): reported as a failure to reach the server.
        let fake = Fake::new(DATA, vec![]);
        *fake.complete.lock().unwrap() = true;
        fake.stored.lock().unwrap().extend_from_slice(DATA);
        fake.head_faults.lock().unwrap().extend((0..11).map(|_| Head::Status(503)));
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Network);
        assert_eq!(fake.log().iter().filter(|l| l.starts_with("PATCH")).count(), 6);
    }

    #[test]
    fn exists_with_a_partial_head_is_a_name_clash() {
        let fake = Fake::new(DATA, vec![]);
        *fake.complete.lock().unwrap() = true;
        fake.head_faults.lock().unwrap().push_back(Head::Partial(3));
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().message, "A file with this name already exists.");
        assert_eq!(fake.log(), ["PATCH 0+10", "HEAD"]);
    }

    #[test]
    fn head_unauthorized_during_retry_fails_as_auth() {
        let fake = Fake::new(DATA, vec![Some(Fault::DropAfter(0))]);
        fake.head_faults.lock().unwrap().push_back(Head::Status(401));
        let (result, _) = run(&fake, false);
        assert_eq!(result.unwrap_err().kind, ErrorKind::Auth);
    }
}
