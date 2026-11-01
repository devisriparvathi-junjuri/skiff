//! Byte-stream seam. Local connect and peer credentials remain in the local adapter.
use tokio::io::{AsyncRead, AsyncWrite};

pub trait Transport: Send + 'static {
    type Reader: AsyncRead + Unpin + Send + 'static;
    type Writer: AsyncWrite + Unpin + Send + 'static;
    fn split(self) -> (Self::Reader, Self::Writer);
}

impl Transport for tokio::net::UnixStream {
    type Reader = tokio::net::unix::OwnedReadHalf;
    type Writer = tokio::net::unix::OwnedWriteHalf;
    fn split(self) -> (Self::Reader, Self::Writer) {
        self.into_split()
    }
}
