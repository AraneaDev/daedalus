use daedalus_proto::remote::{SshTarget, SshTunnel};
use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, Request, ResponsePayload, ServerEvent, ServerMessage, read_frame, write_frame,
};
use std::path::Path;

pub type ClientError = Box<dyn std::error::Error>;

pub struct DaemonClient {
    stream: IpcStream,
    next_id: u64,
    _tunnel: Option<SshTunnel>,
}

impl DaemonClient {
    #[allow(dead_code)]
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        Self::connect_with_auth(socket, None).await
    }

    pub async fn connect_with_auth(
        socket: &Path,
        auth_token: Option<&str>,
    ) -> Result<Self, ClientError> {
        let socket_str = socket.to_string_lossy();
        let (stream, tunnel) = if socket_str.starts_with("ssh://") {
            let target = SshTarget::parse(&socket_str)?;
            let tunnel = SshTunnel::start(target).await?;
            let stream = IpcStream::connect(tunnel.local_endpoint()).await?;
            (stream, Some(tunnel))
        } else {
            (IpcStream::connect(socket).await?, None)
        };

        let mut client = Self {
            stream,
            next_id: 1,
            _tunnel: tunnel,
        };

        if let Some(token) = auth_token.filter(|t| !t.is_empty()) {
            match client
                .request(ClientMessage::Authenticate {
                    token: token.to_string(),
                })
                .await?
            {
                ResponsePayload::Ok => {}
                other => return Err(format!("unexpected authenticate response: {other:?}").into()),
            }
        }

        Ok(client)
    }

    pub async fn request(
        &mut self,
        payload: ClientMessage,
    ) -> Result<ResponsePayload, ClientError> {
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
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "daemon closed connection",
                    )
                    .into());
                }
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
