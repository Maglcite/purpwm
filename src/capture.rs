//! Private, bounded screenshot transport used by the desktop portal backend.

use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

pub type CaptureReply = mpsc::Sender<Result<Arc<Vec<u8>>, String>>;

struct Client {
    stream: UnixStream,
    received: Vec<u8>,
    since: Instant,
}

pub struct CaptureServer {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<Client>,
    in_flight: Arc<AtomicUsize>,
}

impl CaptureServer {
    pub fn bind(path: &Path) -> io::Result<Self> {
        // The Wayland socket name is unique to this compositor; don't replace live sockets.
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "capture socket is live",
                ));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_owned(),
            clients: Vec::new(),
            in_flight: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Never blocks the renderer, including when a peer sends a partial command.
    pub fn poll(&mut self, locked: bool) -> Vec<CaptureReply> {
        for _ in 0..8 {
            match self.listener.accept() {
                Ok((stream, _))
                    if self.clients.len() + self.in_flight.load(Ordering::Relaxed) < 8 =>
                {
                    if stream.set_nonblocking(true).is_ok() {
                        self.clients.push(Client {
                            stream,
                            received: Vec::new(),
                            since: Instant::now(),
                        });
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let mut requests = Vec::new();
        let mut index = 0;
        while index < self.clients.len() {
            let client = &mut self.clients[index];
            let mut bytes = [0u8; 16];
            let done = match client.stream.read(&mut bytes) {
                Ok(0) => true,
                Ok(count) => {
                    client.received.extend_from_slice(&bytes[..count]);
                    client.received.contains(&b'\n') || client.received.len() >= 16
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => false,
                Err(_) => true,
            };
            if !done && client.since.elapsed() < Duration::from_secs(2) {
                index += 1;
                continue;
            }
            let client = self.clients.swap_remove(index);
            let valid = client.received == b"capture\n";
            let (tx, rx) = mpsc::channel::<Result<Arc<Vec<u8>>, String>>();
            if valid && !locked {
                requests.push(tx);
            } else {
                let _ = tx.send(Err(if locked {
                    "session is locked"
                } else {
                    "invalid capture request"
                }
                .into()));
            }
            let in_flight = Arc::clone(&self.in_flight);
            in_flight.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let mut stream = client.stream;
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(png)) => {
                        if writeln!(stream, "OK {}", png.len()).is_ok() {
                            let _ = stream.write_all(&png);
                        }
                    }
                    _ => {
                        let _ = stream.write_all(b"ERR capture unavailable\n");
                    }
                }
                in_flight.fetch_sub(1, Ordering::Relaxed);
            });
        }
        requests
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Encoding and socket writes stay off the compositor thread.
pub fn deliver(replies: Vec<CaptureReply>, capture: Result<(u32, u32, Vec<u8>), String>) {
    if replies.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let result = capture.and_then(|(width, height, rgba)| {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .and_then(|mut writer| writer.write_image_data(&rgba))
                .map_err(|error| error.to_string())?;
            Ok(Arc::new(bytes))
        });
        for reply in replies {
            let _ = reply.send(result.clone());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_transport_handles_partial_commands_and_denies_locked_sessions() {
        let directory = std::env::temp_dir().join(format!("purpwm-capture-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("capture.sock");
        let mut server = CaptureServer::bind(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(b"cap").unwrap();
        assert!(server.poll(false).is_empty());
        client.write_all(b"ture\n").unwrap();
        let requests = server.poll(false);
        assert_eq!(requests.len(), 1);
        deliver(requests, Ok((1, 1, vec![12, 34, 56, 255])));
        let mut result = Vec::new();
        client.read_to_end(&mut result).unwrap();
        let header = result.iter().position(|byte| *byte == b'\n').unwrap();
        let image = png::Decoder::new(&result[header + 1..])
            .read_info()
            .unwrap();
        assert_eq!((image.info().width, image.info().height), (1, 1));
        let mut denied = UnixStream::connect(&path).unwrap();
        denied
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        denied.write_all(b"capture\n").unwrap();
        assert!(server.poll(true).is_empty());
        let mut message = String::new();
        denied.read_to_string(&mut message).unwrap();
        assert!(message.starts_with("ERR "));
        drop(server);
        assert!(!path.exists());
        std::fs::remove_dir(directory).unwrap();
    }
}
