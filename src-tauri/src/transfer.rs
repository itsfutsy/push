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
