use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, Request, ResponsePayload, ServerEvent, ServerMessage, read_frame, write_frame,
};
use std::path::Path;

pub struct DaemonClient {
    stream: IpcStream,
    next_id: u64,
}

impl DaemonClient {
    pub async fn connect(socket: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let stream = IpcStream::connect(socket).await?;
        Ok(Self { stream, next_id: 1 })
    }

    pub async fn request(
        &mut self,
        payload: ClientMessage,
    ) -> Result<ResponsePayload, Box<dyn std::error::Error>> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(&mut self.stream, &Request { id, payload }).await?;
        loop {
            match read_frame::<_, ServerMessage>(&mut self.stream).await? {
                Some(ServerMessage::Response { id: rid, result }) if rid == id => {
                    return Ok(result?);
                }
                Some(ServerMessage::Event(_)) => continue,
                Some(other) => return Err(format!("unexpected message: {other:?}").into()),
                None => return Err("daemon closed connection".into()),
            }
        }
    }

    pub async fn next_event(&mut self) -> Result<Option<ServerEvent>, Box<dyn std::error::Error>> {
        loop {
            match read_frame::<_, ServerMessage>(&mut self.stream).await? {
                Some(ServerMessage::Event(evt)) => return Ok(Some(evt)),
                Some(ServerMessage::Response { .. }) => continue,
                None => return Ok(None),
            }
        }
    }
}
