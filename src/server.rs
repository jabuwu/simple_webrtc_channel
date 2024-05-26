use std::net::SocketAddr;

use crate::{Configuration, DataChannel};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerError {
    FailedToListen(SocketAddr),
    FailedToBind(SocketAddr),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FailedToListen(address) => {
                f.write_str(&format!("Failed to listen on TCP {}", address))
            }
            Self::FailedToBind(address) => {
                f.write_str(&format!("Failed to listen on bind on UDP {}", address))
            }
        }
    }
}

impl std::error::Error for ServerError {}

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
    pub fn new(
        http_address: SocketAddr,
        udp_address: SocketAddr,
        nat_ips: Vec<String>,
        webrtc_configuration: Configuration,
    ) -> Result<Self, ServerError> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use crate::{Signal, Signaler, SignalerKind};
            use std::{
                net::UdpSocket,
                sync::{mpsc, Arc},
                time::{Duration, Instant},
            };
            use tiny_http::{Header, Response, Server};
            use tokio::sync::RwLock;
            use webrtc::{
                ice::{
                    udp_mux::{UDPMuxDefault, UDPMuxParams},
                    udp_network::UDPNetwork,
                },
                ice_transport::ice_candidate_type::RTCIceCandidateType,
            };
            struct MuxData {
                socket: Option<UdpSocket>,
                mux_default: Option<Arc<webrtc::ice::udp_mux::UDPMuxDefault>>,
            }
            let server = Server::http(http_address)
                .map_err(|_| ServerError::FailedToListen(http_address))?;
            let socket =
                UdpSocket::bind(udp_address).map_err(|_| ServerError::FailedToBind(udp_address))?;
            socket
                .set_nonblocking(true)
                .map_err(|_| ServerError::FailedToBind(udp_address))?;
            let mux_data = Arc::new(RwLock::new(MuxData {
                socket: Some(socket),
                mux_default: None,
            }));
            let (connection_sender, connection_receiver) = mpsc::channel();
            std::thread::spawn(move || {
                for mut request in server.incoming_requests() {
                    let mut offer = String::new();
                    if request.as_reader().read_to_string(&mut offer).is_err() {
                        _ = request.respond(Response::from_string("").with_status_code(500));
                        continue;
                    }
                    let mux_data = mux_data.clone();
                    let nat_ips = nat_ips.clone();
                    let mut signaler = Signaler::new_with_setting_engine(
                        webrtc_configuration.clone(),
                        SignalerKind::Answer,
                        async move {
                            use tokio::net::UdpSocket;
                            use webrtc::api::setting_engine::SettingEngine;
                            let mut mux_data = mux_data.write().await;
                            let mux_default = if let Some(mux_default) =
                                mux_data.mux_default.as_ref().cloned()
                            {
                                mux_default
                            } else {
                                let udp_socket = UdpSocket::try_from(
                                    mux_data
                                        .socket
                                        .take()
                                        .expect("Expected a UdpSocket to convert into Tokio."),
                                )
                                .expect("Failed to create Tokio UdpSocket from std UdpSocket.");
                                let mux_default = UDPMuxDefault::new(UDPMuxParams::new(udp_socket));
                                mux_data.mux_default = Some(mux_default.clone());
                                mux_default
                            };
                            let mut setting_engine = SettingEngine::default();
                            setting_engine.set_udp_network(UDPNetwork::Muxed(mux_default));
                            setting_engine
                                .set_nat_1to1_ips(nat_ips.clone(), RTCIceCandidateType::Host);
                            setting_engine
                        },
                    );
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
            Ok(Self {
                connection_receiver,
                connections: vec![],
            })
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
