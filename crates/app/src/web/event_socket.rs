//! Workspace observations on connections independent of the browser HTTP/1 request pool.
//! Uses the same projection and admission as SSE. This channel accepts no commands.
use super::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use futures_util::{SinkExt, StreamExt};

pub(super) async fn open(Scoped(shared): Scoped, upgrade: WebSocketUpgrade) -> Response {
    let Ok(global) = shared.event_sockets.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "event socket limit reached",
        )
            .into_response();
    };
    let Ok(permit) = shared.clients.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "event client limit reached",
        )
            .into_response();
    };
    let client = conversations::Client::new(shared.projections, shared.live.subscribe(), permit);
    let stopped = shared.stopped;
    // Register before upgrading so server shutdown joins even a pending upgrade callback.
    let tracked = shared.event_streams.token();
    upgrade
        .max_message_size(1024)
        .max_frame_size(1024)
        .on_upgrade(move |socket| async move {
            let _tracked = tracked;
            let _global = global;
            serve(socket, client, stopped).await;
        })
}

async fn serve(socket: WebSocket, mut client: conversations::Client, stopped: CancellationToken) {
    let (mut writer, mut reader) = socket.split();
    let mut sequence = 0u64;
    loop {
        let frame = tokio::select! {
            biased;
            _ = stopped.cancelled() => break,
            incoming = reader.next() => match incoming {
                Some(Ok(Message::Ping(bytes))) => {
                    if writer.send(Message::Pong(bytes)).await.is_err() { break; }
                    continue;
                },
                Some(Ok(Message::Pong(_))) => continue,
                _ => break,
            },
            frame = client.batch(sequence) => match frame { Some(frame) => frame, None => break },
        };
        let delivered = async {
            writer.send(Message::Text(frame.into())).await.ok()?;
            loop {
                match reader.next().await? {
                    Ok(Message::Text(text)) if text == format!("ack:{sequence}") => return Some(()),
                    Ok(Message::Ping(bytes)) => writer.send(Message::Pong(bytes)).await.ok()?,
                    Ok(Message::Pong(_)) => {}
                    _ => return None,
                }
            }
        };
        // A slow/hidden/disconnected UI cannot enqueue unlimited browser messages. While
        // awaiting its receipt the watch channel retains just the newest projection.
        let received = tokio::select! {
            _ = stopped.cancelled() => false,
            result = tokio::time::timeout(Duration::from_secs(5), delivered) => matches!(result, Ok(Some(()))),
        };
        if !received {
            break;
        }
        let Some(next) = sequence.checked_add(1) else {
            break;
        };
        sequence = next;
    }
}
