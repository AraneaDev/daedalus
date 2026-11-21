use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("frame length {0} exceeds maximum allowed {MAX_FRAME_SIZE}")]
    FrameTooLarge(usize),
    #[error("unexpected EOF")]
    Eof,
    #[error("serialization error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("deserialization error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), CodecError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = rmp_serde::to_vec_named(value)?;
    if payload.len() > MAX_FRAME_SIZE {
        return Err(CodecError::FrameTooLarge(payload.len()));
    }
    let len_bytes = (payload.len() as u32).to_be_bytes();
    writer.write_all(&len_bytes).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, CodecError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(CodecError::Io(err)),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(CodecError::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await.map_err(|err| {
        if err.kind() == std::io::ErrorKind::UnexpectedEof {
            CodecError::Eof
        } else {
            CodecError::Io(err)
        }
    })?;
    let value = rmp_serde::from_slice(&payload)?;
    Ok(Some(value))
}
