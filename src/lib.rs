pub enum Signal {
    Offer(String),
    Answer(String),
    IceCandidate(Option<String>),
}

pub struct Signaler {
    #[cfg(not(target_arch = "wasm32"))]
    error: std::sync::Arc<tokio::sync::RwLock<Option<Error>>>,
    #[cfg(not(target_arch = "wasm32"))]
    signal_receiver: tokio::sync::mpsc::Receiver<Signal>,
    #[cfg(not(target_arch = "wasm32"))]
    signal_sender: Option<tokio::sync::mpsc::Sender<Signal>>,
    #[cfg(not(target_arch = "wasm32"))]
    channel_receiver: tokio::sync::mpsc::Receiver<DataChannel>,
    #[cfg(target_arch = "wasm32")]
    error: std::sync::Arc<std::sync::RwLock<Option<Error>>>,
    #[cfg(target_arch = "wasm32")]
    wake_up: std::sync::Arc<std::sync::RwLock<Option<js_sys::Function>>>,
    #[cfg(target_arch = "wasm32")]
    signal_receiver: std::sync::mpsc::Receiver<Signal>,
    #[cfg(target_arch = "wasm32")]
    signal_sender: Option<std::sync::mpsc::Sender<Signal>>,
    #[cfg(target_arch = "wasm32")]
    channel_receiver: std::sync::mpsc::Receiver<DataChannel>,
}

impl Signaler {
    // TODO: config
    pub fn new(offer: bool) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::{sync::Arc, thread::spawn};
            use tokio::sync::{mpsc, RwLock};
            let (signal_sender, signal_to_thread) = mpsc::channel::<Signal>(100);
            let (signal_from_thread, signal_receiver) = mpsc::channel::<Signal>(100);
            let (channel_sender, channel_receiver) = mpsc::channel::<DataChannel>(100);
            let error = Arc::new(RwLock::new(None));
            let error_clone = error.clone();
            spawn(move || {
                let mut signal_receiver = signal_to_thread;
                let signal_sender = signal_from_thread;
                let error = error_clone;
                let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                else {
                    *error.blocking_write() = Some(Error::FailedToStartRuntime);
                    return;
                };
                runtime.block_on(async move {
                    use webrtc::{
                        api::{
                            interceptor_registry::register_default_interceptors,
                            media_engine::MediaEngine, APIBuilder,
                        },
                        data_channel::data_channel_init::RTCDataChannelInit,
                        ice_transport::{
                            ice_candidate::RTCIceCandidateInit,
                            ice_credential_type::RTCIceCredentialType, ice_server::RTCIceServer,
                        },
                        interceptor::registry::Registry,
                        peer_connection::{
                            configuration::RTCConfiguration,
                            peer_connection_state::RTCPeerConnectionState,
                            sdp::session_description::RTCSessionDescription,
                        },
                    };
                    let mut media_engine = MediaEngine::default();
                    if media_engine.register_default_codecs().is_err() {
                        error
                            .write()
                            .await
                            .get_or_insert(Error::FailedToStartRuntime);
                        return;
                    }
                    let registry = Registry::new();
                    let Ok(registry) = register_default_interceptors(registry, &mut media_engine)
                    else {
                        error
                            .write()
                            .await
                            .get_or_insert(Error::FailedToStartRuntime);
                        return;
                    };
                    let api = APIBuilder::new()
                        .with_media_engine(media_engine)
                        .with_interceptor_registry(registry);
                    let api = api.build();
                    let configuration = RTCConfiguration::from(RTCConfiguration {
                        ice_servers: vec![RTCIceServer {
                            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
                            username: String::new(),
                            credential: String::new(),
                            credential_type: RTCIceCredentialType::Unspecified,
                        }],
                        ..Default::default()
                    });
                    let Ok(peer) = api.new_peer_connection(configuration).await else {
                        error.write().await.get_or_insert(Error::FailedToCreatePeer);
                        return;
                    };
                    {
                        let error = error.clone();
                        peer.on_peer_connection_state_change(Box::new(move |state| {
                            let error = error.clone();
                            Box::pin(async move {
                                if state == RTCPeerConnectionState::Disconnected {
                                    error.write().await.get_or_insert(Error::Disconnected);
                                }
                            })
                        }));
                    }
                    let open_data_channel = Arc::new(RwLock::new(None));
                    if offer {
                        let Ok(data_channel) = peer
                            .create_data_channel(
                                "data",
                                Some(RTCDataChannelInit {
                                    ordered: Some(false),
                                    max_retransmits: Some(0),
                                    ..Default::default()
                                }),
                            )
                            .await
                        else {
                            error
                                .write()
                                .await
                                .get_or_insert(Error::FailedToCreateDataChannel);
                            return;
                        };
                        let channel = data_channel.clone();
                        let open_data_channel = open_data_channel.clone();
                        let error_clone = error.clone();
                        data_channel.on_open(Box::new(move || {
                            let error = error_clone.clone();
                            Box::pin(async move {
                                let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                _ = channel_sender
                                    .send(DataChannel {
                                        error,
                                        message_sender: obj_to_rtc_sender,
                                        message_receiver: rtc_to_obj_receiver,
                                    })
                                    .await;
                                *open_data_channel.write().await =
                                    Some((channel.clone(), obj_to_rtc_receiver));
                                let message_sender = rtc_to_obj_sender;
                                channel.on_message(Box::new(move |message| {
                                    let message_sender = message_sender.clone();
                                    Box::pin(async move {
                                        _ = message_sender
                                            .send(message.data.into_iter().collect::<Vec<_>>())
                                            .await;
                                    })
                                }));
                            })
                        }));
                        let Ok(offer) = peer.create_offer(None).await else {
                            error
                                .write()
                                .await
                                .get_or_insert(Error::FailedToCreateOffer);
                            return;
                        };
                        if peer.set_local_description(offer.clone()).await.is_err() {
                            error
                                .write()
                                .await
                                .get_or_insert(Error::FailedToSetLocalDescription);
                            return;
                        }
                        _ = signal_sender.send(Signal::Offer(offer.sdp)).await;
                    } else {
                        let open_data_channel = open_data_channel.clone();
                        let error_clone = error.clone();
                        peer.on_data_channel(Box::new(move |channel| {
                            let error = error_clone.clone();
                            let channel_sender = channel_sender.clone();
                            let open_data_channel = open_data_channel.clone();
                            Box::pin(async move {
                                let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                _ = channel_sender
                                    .send(DataChannel {
                                        error,
                                        message_sender: obj_to_rtc_sender,
                                        message_receiver: rtc_to_obj_receiver,
                                    })
                                    .await;
                                *open_data_channel.write().await =
                                    Some((channel.clone(), obj_to_rtc_receiver));
                                let message_sender = rtc_to_obj_sender;
                                channel.on_message(Box::new(move |message| {
                                    let message_sender = message_sender.clone();
                                    Box::pin(async move {
                                        _ = message_sender
                                            .send(message.data.into_iter().collect::<Vec<_>>())
                                            .await;
                                    })
                                }));
                            })
                        }));
                    }
                    {
                        let signal_sender = signal_sender.clone();
                        peer.on_ice_candidate(Box::new(move |ice_candidate| {
                            let signal_sender = signal_sender.clone();
                            Box::pin(async move {
                                if let Some(ice_candidate) = ice_candidate {
                                    if let Ok(json) = ice_candidate.to_json() {
                                        _ = signal_sender
                                            .send(Signal::IceCandidate(Some(json.candidate)))
                                            .await;
                                    }
                                } else {
                                    _ = signal_sender.send(Signal::IceCandidate(None)).await;
                                }
                            })
                        }));
                    }
                    while let Some(signal) = signal_receiver.recv().await {
                        match signal {
                            Signal::Offer(sdp) => {
                                let Ok(offer) = RTCSessionDescription::offer(sdp) else {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToCreateOfferDescription);
                                    return;
                                };
                                if peer.set_remote_description(offer).await.is_err() {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToSetRemoteDescription);
                                    return;
                                }
                                let Ok(answer) = peer.create_answer(None).await else {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToCreateAnswer);
                                    return;
                                };
                                if peer.set_local_description(answer.clone()).await.is_err() {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToSetLocalDescription);
                                    return;
                                }
                                _ = signal_sender.send(Signal::Answer(answer.sdp)).await;
                            }
                            Signal::Answer(sdp) => {
                                let Ok(answer) = RTCSessionDescription::answer(sdp) else {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToCreateAnswerDescription);
                                    return;
                                };
                                if peer.set_remote_description(answer).await.is_err() {
                                    error
                                        .write()
                                        .await
                                        .get_or_insert(Error::FailedToSetRemoteDescription);
                                    return;
                                }
                            }
                            Signal::IceCandidate(ice_candidate) => {
                                if let Some(ice_candidate) = ice_candidate {
                                    if peer
                                        .add_ice_candidate(RTCIceCandidateInit {
                                            candidate: ice_candidate,
                                            ..Default::default()
                                        })
                                        .await
                                        .is_err()
                                    {
                                        error
                                            .write()
                                            .await
                                            .get_or_insert(Error::FailedToAddIceCandidate);
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    let Some((data_channel, mut message_receiver)) =
                        open_data_channel.write().await.take()
                    else {
                        return;
                    };
                    while let Some(message) = message_receiver.recv().await {
                        _ = data_channel.send(&message.into()).await;
                    }
                    _ = peer.close();
                });
            });
            Self {
                error,
                signal_receiver,
                signal_sender: Some(signal_sender),
                channel_receiver,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            use js_sys::{Array, Object, Promise, Reflect, Uint8Array};
            use std::sync::{
                mpsc::{self, TryRecvError},
                Arc, RwLock,
            };
            use wasm_bindgen::{closure::Closure, JsCast, JsValue};
            use wasm_bindgen_futures::spawn_local;
            use wasm_bindgen_futures::JsFuture;
            use web_sys::{
                RtcConfiguration, RtcDataChannel, RtcDataChannelInit, RtcDataChannelType,
                RtcIceCandidateInit, RtcIceTransportPolicy, RtcPeerConnection, RtcSdpType,
                RtcSessionDescription, RtcSessionDescriptionInit, TextEncoder,
            };
            let error = Arc::new(RwLock::new(None));
            let (signal_sender, signal_to_thread) = mpsc::channel::<Signal>();
            let (signal_from_thread, signal_receiver) = mpsc::channel::<Signal>();
            let (channel_sender, channel_receiver) = mpsc::channel::<DataChannel>();
            let wake_up = Arc::new(RwLock::new(None));
            let wake_up_cloned = wake_up.clone();
            let error_clone = error.clone();
            spawn_local(async move {
                let mut closures = vec![];
                let error = error_clone;
                let signal_receiver = signal_to_thread;
                let signal_sender = signal_from_thread;
                let wake_up = wake_up_cloned;
                let mut configuration = RtcConfiguration::new();
                let ice_servers = Array::new();
                let mut ice_server = Object::new();
                let urls = Array::new();
                urls.push(&JsValue::from("stun:stun.l.google.com:19302"));
                Reflect::set(&mut ice_server, &JsValue::from("urls"), &urls)
                    .expect("Should set ice server urls.");
                configuration.ice_servers(&ice_servers);
                configuration.ice_transport_policy(RtcIceTransportPolicy::All);
                let Ok(peer) = RtcPeerConnection::new_with_configuration(&configuration) else {
                    if let Ok(mut error) = error.write() {
                        error.get_or_insert(Error::FailedToCreatePeer);
                    }
                    return;
                };
                {
                    let peer_clone = peer.clone();
                    let error = error.clone();
                    let onconnectionstatechange = Closure::wrap(Box::new(move |_: JsValue| {
                        let connection_state =
                            Reflect::get(&peer_clone, &JsValue::from("connectionState")).expect(
                                "Expected connectionState on onconnectionstatechange callback.",
                            );
                        if connection_state == JsValue::from("disconnected") {
                            if let Ok(mut error) = error.write() {
                                error.get_or_insert(Error::Disconnected);
                            }
                        }
                    })
                        as Box<dyn Fn(JsValue)>);
                    peer.set_onconnectionstatechange(Some(
                        onconnectionstatechange.as_ref().unchecked_ref(),
                    ));
                    closures.push(onconnectionstatechange);
                }
                // TODO: on_peer_connection_state_change
                let open_data_channel = Arc::new(RwLock::new(None));
                if offer {
                    let mut data_channel_init = RtcDataChannelInit::new();
                    data_channel_init.ordered(false);
                    data_channel_init.max_retransmits(0);
                    let data_channel =
                        peer.create_data_channel_with_data_channel_dict("data", &data_channel_init);
                    data_channel.set_binary_type(RtcDataChannelType::Arraybuffer);
                    let open_data_channel = open_data_channel.clone();
                    let wake_up = wake_up.clone();
                    let error_clone = error.clone();
                    let onopen = Closure::wrap(Box::new(move |event: JsValue| {
                        let error = error_clone.clone();
                        let channel = RtcDataChannel::from(
                            Reflect::get(&event, &JsValue::from("target"))
                                .expect("Expected target on onopen callback."),
                        );
                        let (obj_to_rtc_sender, obj_to_rtc_receiver) = mpsc::channel::<Vec<u8>>();
                        let (rtc_to_obj_sender, rtc_to_obj_receiver) = mpsc::channel::<Vec<u8>>();
                        if let Ok(mut open_data_channel) = open_data_channel.write() {
                            *open_data_channel = Some((channel.clone(), obj_to_rtc_receiver));
                        }
                        _ = channel_sender.send(DataChannel {
                            error,
                            wake_up: wake_up.clone(),
                            message_sender: obj_to_rtc_sender,
                            message_receiver: rtc_to_obj_receiver,
                        });
                        let message_sender = rtc_to_obj_sender;
                        let onmessage = Closure::wrap(Box::new(move |event: JsValue| {
                            let data = Reflect::get(&event, &"data".into())
                                .expect("Expected data on onmessage callback.");
                            let is_string = data.is_string();
                            if is_string {
                                if let Ok(encoder) = TextEncoder::new() {
                                    if let Some(data_string) = data.as_string() {
                                        _ = message_sender
                                            .send(encoder.encode_with_input(data_string.as_str()));
                                    }
                                }
                            } else {
                                _ = message_sender.send(Uint8Array::new(&data).to_vec());
                            }
                        })
                            as Box<dyn Fn(JsValue)>);
                        channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
                        onmessage.forget(); // TODO: don't forget
                    }) as Box<dyn Fn(JsValue)>);
                    data_channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));
                    closures.push(onopen);
                    let Ok(offer) = JsFuture::from(peer.create_offer()).await else {
                        if let Ok(mut error) = error.write() {
                            error.get_or_insert(Error::FailedToCreateOffer);
                        }
                        return;
                    };
                    let offer = RtcSessionDescription::from(offer);
                    let mut offer_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                    offer_init.sdp(&offer.sdp());
                    if JsFuture::from(peer.set_local_description(&offer_init))
                        .await
                        .is_err()
                    {
                        if let Ok(mut error) = error.write() {
                            error.get_or_insert(Error::FailedToSetLocalDescription);
                        }
                        return;
                    }
                    _ = signal_sender.send(Signal::Offer(offer.sdp()));
                } else {
                    let open_data_channel = open_data_channel.clone();
                    let wake_up = wake_up.clone();
                    let error = error.clone();
                    let ondatachannel = Closure::wrap(Box::new(move |event: JsValue| {
                        let channel = js_sys::Reflect::get(&event, &"channel".into())
                            .expect("Expected channel on ondatachannel callback.");
                        let data_channel = RtcDataChannel::from(channel);
                        data_channel.set_binary_type(RtcDataChannelType::Arraybuffer);
                        let open_data_channel = open_data_channel.clone();
                        let channel_sender = channel_sender.clone();
                        let wake_up = wake_up.clone();
                        let error = error.clone();
                        let onopen = Closure::wrap(Box::new(move |event: JsValue| {
                            let channel = RtcDataChannel::from(
                                Reflect::get(&event, &JsValue::from("target"))
                                    .expect("Expected target on onopen callback."),
                            );
                            let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                mpsc::channel::<Vec<u8>>();
                            let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                mpsc::channel::<Vec<u8>>();
                            if let Ok(mut open_data_channel) = open_data_channel.write() {
                                *open_data_channel = Some((channel.clone(), obj_to_rtc_receiver));
                            }
                            _ = channel_sender.send(DataChannel {
                                error: error.clone(),
                                wake_up: wake_up.clone(),
                                message_sender: obj_to_rtc_sender,
                                message_receiver: rtc_to_obj_receiver,
                            });
                            let message_sender = rtc_to_obj_sender;
                            let onmessage = Closure::wrap(Box::new(move |event: JsValue| {
                                let data = Reflect::get(&event, &"data".into())
                                    .expect("Expected data on onmessage callback.");
                                let is_string = data.is_string();
                                if is_string {
                                    if let Ok(encoder) = TextEncoder::new() {
                                        if let Some(data_string) = data.as_string() {
                                            _ = message_sender.send(
                                                encoder.encode_with_input(data_string.as_str()),
                                            );
                                        }
                                    }
                                } else {
                                    _ = message_sender.send(Uint8Array::new(&data).to_vec());
                                }
                            })
                                as Box<dyn Fn(JsValue)>);
                            channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
                            onmessage.forget(); // TODO: don't forget
                        })
                            as Box<dyn Fn(JsValue)>);
                        data_channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));
                        onopen.forget(); // TODO: don't forget
                    })
                        as Box<dyn Fn(JsValue)>);
                    peer.set_ondatachannel(Some(ondatachannel.as_ref().unchecked_ref()));
                    closures.push(ondatachannel);
                }
                {
                    let signal_sender = signal_sender.clone();
                    let onicecandidate = Closure::wrap(Box::new(move |ice_candidate: JsValue| {
                        let candidate = Reflect::get(&ice_candidate, &"candidate".into())
                            .expect("Expected candidate on onicecandidate callback");
                        _ = signal_sender.send(Signal::IceCandidate(if candidate.is_object() {
                            Some(
                                Reflect::get(&candidate, &"candidate".into())
                                    .expect("Expected candidate in ice candidate")
                                    .as_string()
                                    .expect("Expected ice candidate to be a string"),
                            )
                        } else {
                            None
                        }));
                    })
                        as Box<dyn Fn(JsValue)>);
                    peer.set_onicecandidate(Some(onicecandidate.as_ref().unchecked_ref()));
                    closures.push(onicecandidate);
                }
                loop {
                    match signal_receiver.try_recv() {
                        Ok(signal) => match signal {
                            Signal::Offer(offer) => {
                                let mut offer_init =
                                    RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                                offer_init.sdp(&offer);
                                if JsFuture::from(peer.set_remote_description(&offer_init))
                                    .await
                                    .is_err()
                                {
                                    if let Ok(mut error) = error.write() {
                                        error.get_or_insert(Error::FailedToSetRemoteDescription);
                                    }
                                    return;
                                }
                                let Ok(answer) = JsFuture::from(peer.create_answer()).await else {
                                    if let Ok(mut error) = error.write() {
                                        error.get_or_insert(Error::FailedToCreateAnswer);
                                    }
                                    return;
                                };
                                let answer = RtcSessionDescription::from(answer);
                                let mut answer_init =
                                    RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                                answer_init.sdp(&answer.sdp());
                                if JsFuture::from(peer.set_local_description(&answer_init))
                                    .await
                                    .is_err()
                                {
                                    if let Ok(mut error) = error.write() {
                                        error.get_or_insert(Error::FailedToSetLocalDescription);
                                    }
                                    return;
                                }
                                _ = signal_sender.send(Signal::Answer(answer.sdp()));
                            }
                            Signal::Answer(answer) => {
                                let mut answer_init =
                                    RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                                answer_init.sdp(&answer);
                                if JsFuture::from(peer.set_remote_description(&answer_init))
                                    .await
                                    .is_err()
                                {
                                    if let Ok(mut error) = error.write() {
                                        error.get_or_insert(Error::FailedToSetRemoteDescription);
                                    }
                                    return;
                                }
                            }
                            Signal::IceCandidate(ice_candidate) => {
                                if let Some(ice_candidate) = ice_candidate {
                                    let mut ice_candidate_init =
                                        RtcIceCandidateInit::new(&ice_candidate);
                                    ice_candidate_init.sdp_m_line_index(Some(0));
                                    ice_candidate_init.sdp_mid(Some("0"));
                                    if JsFuture::from(
                                        peer.add_ice_candidate_with_opt_rtc_ice_candidate_init(
                                            Some(&ice_candidate_init),
                                        ),
                                    )
                                    .await
                                    .is_err()
                                    {
                                        if let Ok(mut error) = error.write() {
                                            error.get_or_insert(Error::FailedToAddIceCandidate);
                                        }
                                        return;
                                    }
                                } else {
                                    if JsFuture::from(
                                        peer.add_ice_candidate_with_opt_rtc_ice_candidate_init(
                                            None,
                                        ),
                                    )
                                    .await
                                    .is_err()
                                    {
                                        if let Ok(mut error) = error.write() {
                                            error.get_or_insert(Error::FailedToAddIceCandidate);
                                        }
                                        return;
                                    }
                                }
                            }
                        },
                        Err(TryRecvError::Empty) => {
                            let promise = Promise::new(&mut |resolve, _reject| {
                                *wake_up.write().expect("Expect wake_up to be valid") =
                                    Some(resolve);
                            });
                            _ = JsFuture::from(promise).await;
                        }
                        Err(TryRecvError::Disconnected) => {
                            break;
                        }
                    }
                }
                let Some((data_channel, message_receiver)) = open_data_channel
                    .write()
                    .ok()
                    .and_then(|mut open_data_channel| open_data_channel.take())
                else {
                    return;
                };
                loop {
                    match message_receiver.try_recv() {
                        Ok(message) => {
                            _ = data_channel.send_with_u8_array(&message);
                        }
                        Err(TryRecvError::Empty) => {
                            let promise = Promise::new(&mut |resolve, _reject| {
                                *wake_up.write().expect("Expect wake_up to be valid") =
                                    Some(resolve);
                            });
                            _ = JsFuture::from(promise).await;
                        }
                        Err(TryRecvError::Disconnected) => {
                            break;
                        }
                    }
                }
                peer.close();
            });
            Self {
                error,
                wake_up,
                signal_receiver,
                signal_sender: Some(signal_sender),
                channel_receiver,
            }
        }
    }

    pub fn signal(&mut self) -> Option<Signal> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.signal_receiver.try_recv().ok()
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.signal_receiver.try_recv().ok()
        }
    }

    pub fn receive(&mut self, signal: Signal) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(signal_sender) = self.signal_sender.as_mut() {
                tokio::task::block_in_place(|| {
                    _ = signal_sender.blocking_send(signal);
                });
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(signal_sender) = self.signal_sender.as_mut() {
                _ = signal_sender.send(signal);
            }
            self.wake_up();
        }
    }

    pub fn data_channel(&mut self) -> Result<Option<DataChannel>, Error> {
        if self.signal_sender.is_none() {
            return Ok(None);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let error = tokio::task::block_in_place(|| *self.error.blocking_read());
            if let Some(error) = error {
                return Err(error);
            }
            if let Some(data_channel) = self.channel_receiver.try_recv().ok() {
                self.signal_sender = None;
                Ok(Some(data_channel))
            } else {
                Ok(None)
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let Ok(error) = self.error.read().map(|error| *error) else {
                return Err(Error::Disconnected);
            };
            if let Some(error) = error {
                return Err(error);
            }
            if let Some(data_channel) = self.channel_receiver.try_recv().ok() {
                self.signal_sender = None;
                self.wake_up();
                Ok(Some(data_channel))
            } else {
                Ok(None)
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn wake_up(&mut self) {
        use wasm_bindgen::JsValue;
        if let Some(wake_up) = self
            .wake_up
            .write()
            .ok()
            .as_mut()
            .and_then(|wake_up| wake_up.take())
        {
            _ = wake_up.call0(&JsValue::null());
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for Signaler {
    fn drop(&mut self) {
        self.wake_up();
    }
}

pub struct DataChannel {
    #[cfg(not(target_arch = "wasm32"))]
    error: std::sync::Arc<tokio::sync::RwLock<Option<Error>>>,
    #[cfg(not(target_arch = "wasm32"))]
    message_sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    #[cfg(not(target_arch = "wasm32"))]
    message_receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    #[cfg(target_arch = "wasm32")]
    error: std::sync::Arc<std::sync::RwLock<Option<Error>>>,
    #[cfg(target_arch = "wasm32")]
    wake_up: std::sync::Arc<std::sync::RwLock<Option<js_sys::Function>>>,
    #[cfg(target_arch = "wasm32")]
    message_sender: std::sync::mpsc::Sender<Vec<u8>>,
    #[cfg(target_arch = "wasm32")]
    message_receiver: std::sync::mpsc::Receiver<Vec<u8>>,
}

impl DataChannel {
    pub fn send(&mut self, message: Vec<u8>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            tokio::task::block_in_place(|| {
                _ = self.message_sender.blocking_send(message);
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            _ = self.message_sender.send(message);
            self.wake_up();
        }
    }

    pub fn receive(&mut self) -> Result<Option<Vec<u8>>, Error> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let error = tokio::task::block_in_place(|| *self.error.blocking_read());
            if let Some(error) = error {
                return Err(error);
            }
            Ok(self.message_receiver.try_recv().ok())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let Ok(error) = self.error.read().map(|error| *error) else {
                return Err(Error::Disconnected);
            };
            if let Some(error) = error {
                return Err(error);
            }
            Ok(self.message_receiver.try_recv().ok())
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn wake_up(&mut self) {
        use wasm_bindgen::JsValue;
        if let Some(wake_up) = self
            .wake_up
            .write()
            .ok()
            .as_mut()
            .and_then(|wake_up| wake_up.take())
        {
            _ = wake_up.call0(&JsValue::null());
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for DataChannel {
    fn drop(&mut self) {
        self.wake_up();
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Error {
    FailedToStartRuntime,
    FailedToCreatePeer,
    FailedToCreateDataChannel,
    FailedToCreateOffer,
    FailedToCreateOfferDescription,
    FailedToCreateAnswer,
    FailedToCreateAnswerDescription,
    FailedToSetLocalDescription,
    FailedToSetRemoteDescription,
    FailedToAddIceCandidate,
    Disconnected,
}
