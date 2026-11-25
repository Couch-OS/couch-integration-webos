//! Dedicated owner for the continuously serviced LG pointer WebSocket.
//!
//! The worker owns only the pointer socket. The calling thread retains the
//! Couch DeviceClient and its SSAP connection, and sends bounded button
//! commands to this worker. A command whose send outcome is uncertain is
//! never replayed.

use crate::Socket;
use std::{
    io,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
};
use tungstenite::{Error as WsError, Message, WebSocket};

const READ_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const COMMAND_CAPACITY: usize = 1;

trait SetReadTimeout {
    fn set_read_timeout(&self, timeout: std::time::Duration) -> io::Result<()>;
}

impl SetReadTimeout for Socket {
    fn set_read_timeout(&self, timeout: std::time::Duration) -> io::Result<()> {
        match self {
            Socket::Plain(stream) => stream.set_read_timeout(Some(timeout)),
            Socket::Tls(stream) => stream.sock.set_read_timeout(Some(timeout)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerError {
    InvalidButton,
    Busy,
    StoppedBeforeSend,
    Transport,
}

enum Command {
    Button {
        name: String,
        reply: mpsc::SyncSender<Result<(), WorkerError>>,
    },
    Shutdown,
}

/// Owns the pointer WebSocket's command channel and worker thread.
pub(super) struct PointerWorker {
    commands: SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl PointerWorker {
    pub(super) fn spawn(mut socket: WebSocket<Socket>) -> Result<Self, WorkerError> {
        socket
            .get_ref()
            .set_read_timeout(READ_POLL)
            .map_err(|_| WorkerError::Transport)?;

        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let worker = thread::Builder::new()
            .name("webos-pointer".into())
            .spawn(move || run(socket, receiver))
            .map_err(|_| WorkerError::Transport)?;

        Ok(Self {
            commands,
            thread: Some(worker),
        })
    }

    pub(super) fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .map(|worker| worker.is_finished())
            .unwrap_or(true)
    }

    pub(super) fn button(&self, name: &str) -> Result<(), WorkerError> {
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'-')
        {
            return Err(WorkerError::InvalidButton);
        }

        if self.is_finished() {
            return Err(WorkerError::StoppedBeforeSend);
        }

        let (reply, result) = mpsc::sync_channel(1);
        self.commands
            .try_send(Command::Button {
                name: name.to_owned(),
                reply,
            })
            .map_err(|error| match error {
                TrySendError::Full(_) => WorkerError::Busy,
                TrySendError::Disconnected(_) => WorkerError::StoppedBeforeSend,
            })?;

        result.recv().unwrap_or(Err(WorkerError::Transport))
    }

    fn shutdown(&mut self) {
        match self.commands.try_send(Command::Shutdown) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(command)) => {
                let _ = self.commands.send(command);
            }
        }
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for PointerWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(mut socket: WebSocket<Socket>, commands: Receiver<Command>) {
    crate::pointer_diagnostics::record("pointer worker started");
    loop {
        match commands.try_recv() {
            Ok(Command::Button { name, reply }) => {
                let message = Message::Text(format!("type:button\nname:{name}\n\n").into());
                let result = socket
                    .send(message)
                    .map(|()| ())
                    .map_err(|_| WorkerError::Transport);
                let failed = result.is_err();
                crate::pointer_diagnostics::record(&format!(
                    "pointer worker button send {} name={name}",
                    if failed { "failed" } else { "succeeded" }
                ));
                let _ = reply.send(result);
                if failed {
                    // A failed send has uncertain delivery. Exit; never replay.
                    break;
                }
            }
            Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => {
                crate::pointer_diagnostics::record("pointer worker shutdown requested");
                let _ = socket.close(None);
                let _ = socket.flush();
                break;
            }
            Err(TryRecvError::Empty) => {}
        }

        match socket.read() {
            Ok(Message::Ping(_)) => {
                // tungstenite queues the automatic Pong when Ping is read.
                crate::pointer_diagnostics::record("pointer worker received peer Ping");
                if socket.flush().is_err() {
                    crate::pointer_diagnostics::record("pointer worker failed to flush Pong");
                    break;
                }
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => {
                // Reading Close queues the close response; flush it before exit.
                crate::pointer_diagnostics::record("pointer worker received peer Close");
                let _ = socket.flush();
                break;
            }
            Ok(Message::Text(_)) | Ok(Message::Binary(_)) | Ok(Message::Frame(_)) => {
                // The LG pointer channel is not expected to carry application
                // data. Keep servicing the stream without treating it as a key.
            }
            Err(WsError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => {
                crate::pointer_diagnostics::record("pointer worker observed closed transport");
                break;
            }
            Err(_) => {
                crate::pointer_diagnostics::record("pointer worker observed transport error");
                break;
            }
        }
    }
    crate::pointer_diagnostics::record("pointer worker stopped");
}

// Compile-time proof for the exact production socket type, including TLS.
#[allow(dead_code)]
fn pointer_socket_must_be_send() {
    fn assert_send<T: Send>() {}
    assert_send::<WebSocket<Socket>>();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connect_socket;
    use std::{
        net::{Shutdown, TcpListener, TcpStream},
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    type ServerSocket = WebSocket<TcpStream>;

    fn connected_pair() -> (WebSocket<Socket>, ServerSocket) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
        });

        let client = connect_socket(
            &format!("ws://{address}/"),
            Arc::new(Mutex::new(Vec::new())),
            Duration::from_secs(2),
        )
        .unwrap();
        (client, server.join().unwrap())
    }

    #[test]
    fn continuously_reads_and_answers_peer_ping_while_idle() {
        let (client, mut server) = connected_pair();
        let mut worker = PointerWorker::spawn(client).unwrap();

        server
            .send(Message::Ping(b"tv-heartbeat".to_vec().into()))
            .unwrap();

        assert!(matches!(
            server.read().unwrap(),
            Message::Pong(payload) if payload.as_ref() == b"tv-heartbeat"
        ));

        worker.shutdown();
    }

    #[test]
    fn sends_button_frames_through_the_worker() {
        let (client, mut server) = connected_pair();
        let worker = PointerWorker::spawn(client).unwrap();

        worker.button("ENTER").unwrap();

        assert!(matches!(
            server.read().unwrap(),
            Message::Text(text) if text.as_str() == "type:button\nname:ENTER\n\n"
        ));
    }

    #[test]
    fn rejects_invalid_button_names_before_queueing() {
        let (client, _server) = connected_pair();
        let worker = PointerWorker::spawn(client).unwrap();

        assert_eq!(
            worker.button("ENTER\nname:HOME"),
            Err(WorkerError::InvalidButton)
        );
    }

    #[test]
    fn observes_peer_close_and_stops_accepting_commands() {
        let (client, mut server) = connected_pair();
        let worker = PointerWorker::spawn(client).unwrap();

        server.close(None).unwrap();
        assert!(matches!(server.read().unwrap(), Message::Close(_)));

        // Allow the worker to leave its receive loop after the close handshake.
        thread::sleep(READ_POLL + Duration::from_millis(25));
        assert_eq!(worker.button("ENTER"), Err(WorkerError::StoppedBeforeSend));
    }

    #[test]
    fn observes_transport_loss_and_stops() {
        let (client, server) = connected_pair();
        let worker = PointerWorker::spawn(client).unwrap();

        server.get_ref().shutdown(Shutdown::Both).unwrap();
        thread::sleep(READ_POLL + Duration::from_millis(25));

        assert_eq!(worker.button("ENTER"), Err(WorkerError::StoppedBeforeSend));
    }

    #[test]
    fn shutdown_sends_close_and_joins_worker() {
        let (client, mut server) = connected_pair();
        let mut worker = PointerWorker::spawn(client).unwrap();

        worker.shutdown();

        assert!(matches!(server.read().unwrap(), Message::Close(_)));
        assert!(worker.thread.is_none());
    }
}
