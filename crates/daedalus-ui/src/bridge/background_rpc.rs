use daedalus_proto::transport::IpcStream;
use daedalus_proto::{
    ClientMessage, Request, ResponsePayload, ServerMessage, read_frame, write_frame,
};
use std::path::Path;
use std::time::Duration;

pub(super) async fn request(
    endpoint: &Path,
    auth_token: Option<&str>,
    connection: &str,
    request: ClientMessage,
) -> Result<ResponsePayload, String> {
    let mut stream = tokio::time::timeout(Duration::from_secs(30), async {
        let mut stream = IpcStream::connect(endpoint)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(token) = auth_token.filter(|token| !token.is_empty()) {
            let response = call(
                &mut stream,
                connection,
                1,
                ClientMessage::Authenticate {
                    token: token.to_owned(),
                },
            )
            .await?;
            if response != ResponsePayload::Ok {
                return Err(format!(
                    "Unexpected authentication response for the {connection} connection."
                ));
            }
        }
        Ok::<_, String>(stream)
    })
    .await
    .map_err(|_| format!("The {connection} connection timed out."))??;

    call(&mut stream, connection, 2, request).await
}

async fn call(
    stream: &mut IpcStream,
    connection: &str,
    id: u64,
    payload: ClientMessage,
) -> Result<ResponsePayload, String> {
    write_frame(stream, &Request { id, payload })
        .await
        .map_err(|error| error.to_string())?;
    match read_frame::<_, ServerMessage>(stream)
        .await
        .map_err(|error| error.to_string())?
    {
        Some(ServerMessage::Response {
            id: response_id,
            result,
        }) if response_id == id => result.map_err(|error| error.to_string()),
        Some(_) => Err(format!("Unexpected reply on the {connection} connection.")),
        None => Err(format!(
            "The {connection} connection closed before reporting a result."
        )),
    }
}
