use std::net::SocketAddr;

use crate::DataChannel;

struct Connection {
    #[cfg(not(target_arch = "wasm32"))]
    signaler: crate::Signaler,
    #[cfg(not(target_arch = "wasm32"))]
    creation: std::time::Instant,
}

pub struct Server {
    #[cfg(not(target_arch = "wasm32"))]
    connection_receiver: std::sync::mpsc::Receiver<Connection>,
    #[cfg(not(target_arch = "wasm32"))]
    connections: Vec<Connection>,
}

impl Server {
    pub fn new(http_address: SocketAddr) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use crate::{Signal, Signaler, SignalerKind};
            use std::{
                sync::mpsc,
                time::{Duration, Instant},
            };
            use tiny_http::{Header, Response, Server};
            let server = Server::http(http_address).unwrap(); // TODO: remove unwrap
            let (connection_sender, connection_receiver) = mpsc::channel();
            std::thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    let mut offer = String::new();
                    if request.as_reader().read_to_string(&mut offer).is_err() {
                        _ = request.respond(Response::from_string("").with_status_code(500));
                        continue;
                    }
                    let mut signaler = Signaler::new(SignalerKind::Answer);
                    signaler.receive(Signal::Offer(offer));
                    let Ok(answer_sdp) = (loop {
                        match signaler.signal() {
                            Ok(Some(Signal::Answer(answer))) => {
                                break Ok(answer);
                            }
                            Ok(Some(_)) => unreachable!(),
                            Ok(None) => {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            Err(err) => {
                                break Err(err);
                            }
                        }
                    }) else {
                        _ = request.respond(Response::from_string("").with_status_code(400));
                        continue;
                    };
                    let mut ice_candidates = vec![];
                    if (loop {
                        match signaler.signal() {
                            Ok(Some(Signal::IceCandidate(Some(ice_candidate)))) => {
                                ice_candidates.push(ice_candidate);
                            }
                            Ok(Some(Signal::IceCandidate(None))) => {
                                break Ok(());
                            }
                            Ok(Some(_)) => unreachable!(),
                            Ok(None) => {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            Err(err) => {
                                break Err(err);
                            }
                        }
                    })
                    .is_err()
                    {
                        _ = request.respond(Response::from_string("").with_status_code(400));
                        continue;
                    }
                    let mut response = format!("{}\0", answer_sdp);
                    for ice_candidate in ice_candidates {
                        response += &format!("{}\0", ice_candidate);
                    }
                    let Ok(cors_header) = Header::from_bytes(
                        "Access-Control-Allow-Origin".as_bytes(),
                        "*".as_bytes(),
                    ) else {
                        _ = request.respond(Response::from_string("").with_status_code(500));
                        continue;
                    };
                    _ = request.respond(Response::from_string(&response).with_header(cors_header));
                    _ = connection_sender.send(Connection {
                        signaler,
                        creation: Instant::now(),
                    });
                }
            });
            Self {
                connection_receiver,
                connections: vec![],
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            unimplemented!()
        }
    }

    pub fn accept(&mut self) -> Option<DataChannel> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::time::Duration;
            while let Some(connection) = self.connection_receiver.try_recv().ok() {
                self.connections.push(connection);
            }
            let mut return_data_channel = None;
            let mut remove_indices = vec![];
            for (index, connection) in self.connections.iter_mut().enumerate() {
                match connection.signaler.data_channel() {
                    Ok(Some(data_channel)) => {
                        return_data_channel = Some(data_channel);
                        remove_indices.push(index);
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        remove_indices.push(index);
                    }
                }
            }
            if remove_indices.is_empty() {
                self.connections
                    .retain(|connection| connection.creation.elapsed() < Duration::from_secs(10));
            }
            for remove_index in remove_indices.into_iter().rev() {
                self.connections.remove(remove_index);
            }
            return_data_channel
        }
        #[cfg(target_arch = "wasm32")]
        {
            unimplemented!()
        }
    }
}
