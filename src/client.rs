use std::sync::mpsc;

use crate::{Configuration, DataChannel, DataChannelConfiguration, Signal, Signaler, SignalerKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckConnectionError {
    BadResponse,
    WebRtcError(crate::Error),
}

impl std::fmt::Display for CheckConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadResponse => f.write_str("Bad response from server."),
            Self::WebRtcError(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for CheckConnectionError {}

pub struct Client {
    server_url: String,
    signaler: Signaler,
    http_response_receiver: Option<mpsc::Receiver<Result<String, CheckConnectionError>>>,
}

impl Client {
    pub fn new(
        server_url: &str,
        webrtc_configuration: Configuration,
        data_channel_configuration: DataChannelConfiguration,
    ) -> Self {
        Self {
            server_url: server_url.to_owned(),
            signaler: Signaler::new(
                webrtc_configuration,
                SignalerKind::Offer {
                    data_channel_configuration,
                },
            ),
            http_response_receiver: None,
        }
    }

    pub fn check_connection(&mut self) -> Result<Option<DataChannel>, CheckConnectionError> {
        if let Some(Signal::Offer(offer)) = self.signaler.signal().map_err(|err| match err {
            crate::Error::Disconnected => {
                CheckConnectionError::WebRtcError(crate::Error::Disconnected)
            }
            err => CheckConnectionError::WebRtcError(err),
        })? {
            let (http_response_sender, http_response_receiver) = mpsc::channel();
            let request = ehttp::Request::post(&self.server_url, offer.as_bytes().to_vec());
            ehttp::fetch(request, move |result: ehttp::Result<ehttp::Response>| {
                if let Ok(result) = result {
                    if let Ok(message) = std::str::from_utf8(&result.bytes) {
                        _ = http_response_sender.send(Ok(message.to_owned()));
                    } else {
                        _ = http_response_sender.send(Err(CheckConnectionError::BadResponse));
                    }
                } else {
                    _ = http_response_sender.send(Err(CheckConnectionError::BadResponse));
                }
            });
            self.http_response_receiver = Some(http_response_receiver);
        }
        match self
            .http_response_receiver
            .as_ref()
            .and_then(|http_response_receiver| http_response_receiver.try_recv().ok())
        {
            Some(Ok(http_response)) => {
                let mut split = http_response.split('\0');
                let answer = split
                    .next()
                    .ok_or_else(|| CheckConnectionError::BadResponse)?
                    .to_owned();
                let ice_candidates = split.filter(|s| !s.is_empty()).collect::<Vec<_>>();
                self.signaler.receive(Signal::Answer(answer));
                for ice_candidate in ice_candidates {
                    self.signaler
                        .receive(Signal::IceCandidate(Some(ice_candidate.to_owned())));
                }
                self.signaler.receive(Signal::IceCandidate(None));
            }
            Some(Err(err)) => {
                return Err(err);
            }
            None => {}
        }
        self.signaler.data_channel().map_err(|err| match err {
            crate::Error::Disconnected => {
                CheckConnectionError::WebRtcError(crate::Error::Disconnected)
            }
            err => CheckConnectionError::WebRtcError(err),
        })
    }
}
