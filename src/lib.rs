pub enum Signal {
    Offer(String),
    Answer(String),
    IceCandidate(Option<String>),
}

pub struct Signaler {
    #[cfg(not(target_arch = "wasm32"))]
    signal_receiver: tokio::sync::mpsc::Receiver<Signal>,
    #[cfg(not(target_arch = "wasm32"))]
    signal_sender: Option<tokio::sync::mpsc::Sender<Signal>>,
    #[cfg(not(target_arch = "wasm32"))]
    channel_receiver: tokio::sync::mpsc::Receiver<DataChannel>,
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
            spawn(move || {
                let mut signal_receiver = signal_to_thread;
                let signal_sender = signal_from_thread;
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap();
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
                            sdp::session_description::RTCSessionDescription,
                        },
                    };
                    let mut media_engine = MediaEngine::default();
                    media_engine.register_default_codecs().unwrap();
                    let mut registry = Registry::new();
                    registry = register_default_interceptors(registry, &mut media_engine).unwrap();
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
                    let peer = api.new_peer_connection(configuration).await.unwrap();
                    let open_data_channel = Arc::new(RwLock::new(None));
                    if offer {
                        let data_channel = peer
                            .create_data_channel(
                                "data",
                                Some(RTCDataChannelInit {
                                    ordered: Some(false),
                                    max_retransmits: Some(0),
                                    ..Default::default()
                                }),
                            )
                            .await
                            .unwrap();
                        let channel = data_channel.clone();
                        let open_data_channel = open_data_channel.clone();
                        data_channel.on_open(Box::new(move || {
                            Box::pin(async move {
                                let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                *open_data_channel.write().await =
                                    Some((channel.clone(), obj_to_rtc_receiver));
                                channel_sender
                                    .send(DataChannel {
                                        message_sender: obj_to_rtc_sender,
                                        message_receiver: rtc_to_obj_receiver,
                                    })
                                    .await
                                    .unwrap();
                                let message_sender = rtc_to_obj_sender;
                                channel.on_message(Box::new(move |message| {
                                    let message_sender = message_sender.clone();
                                    Box::pin(async move {
                                        message_sender
                                            .send(message.data.into_iter().collect::<Vec<_>>())
                                            .await
                                            .unwrap();
                                    })
                                }));
                            })
                        }));
                        let offer = peer.create_offer(None).await.unwrap();
                        peer.set_local_description(offer.clone()).await.unwrap();
                        signal_sender.send(Signal::Offer(offer.sdp)).await.unwrap();
                    } else {
                        let open_data_channel = open_data_channel.clone();
                        peer.on_data_channel(Box::new(move |channel| {
                            let channel_sender = channel_sender.clone();
                            let open_data_channel = open_data_channel.clone();
                            Box::pin(async move {
                                let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                    mpsc::channel::<Vec<u8>>(100);
                                *open_data_channel.write().await =
                                    Some((channel.clone(), obj_to_rtc_receiver));
                                channel_sender
                                    .send(DataChannel {
                                        message_sender: obj_to_rtc_sender,
                                        message_receiver: rtc_to_obj_receiver,
                                    })
                                    .await
                                    .unwrap();
                                let message_sender = rtc_to_obj_sender;
                                channel.on_message(Box::new(move |message| {
                                    let message_sender = message_sender.clone();
                                    Box::pin(async move {
                                        message_sender
                                            .send(message.data.into_iter().collect::<Vec<_>>())
                                            .await
                                            .unwrap();
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
                                signal_sender
                                    .send(Signal::IceCandidate(ice_candidate.map(
                                        |ice_candidate| ice_candidate.to_json().unwrap().candidate,
                                    )))
                                    .await
                                    .unwrap();
                            })
                        }));
                    }
                    while let Some(signal) = signal_receiver.recv().await {
                        match signal {
                            Signal::Offer(sdp) => {
                                peer.set_remote_description(
                                    RTCSessionDescription::offer(sdp).unwrap(),
                                )
                                .await
                                .unwrap();
                                let answer = peer.create_answer(None).await.unwrap();
                                peer.set_local_description(answer.clone()).await.unwrap();
                                signal_sender
                                    .send(Signal::Answer(answer.sdp))
                                    .await
                                    .unwrap();
                            }
                            Signal::Answer(sdp) => {
                                peer.set_remote_description(
                                    RTCSessionDescription::answer(sdp).unwrap(),
                                )
                                .await
                                .unwrap();
                            }
                            Signal::IceCandidate(ice_candidate) => {
                                if let Some(ice_candidate) = ice_candidate {
                                    peer.add_ice_candidate(RTCIceCandidateInit {
                                        candidate: ice_candidate,
                                        ..Default::default()
                                    })
                                    .await
                                    .unwrap();
                                }
                            }
                        }
                    }
                    let (data_channel, mut message_receiver) =
                        open_data_channel.write().await.take().unwrap();
                    // TODO: onclose
                    while let Some(message) = message_receiver.recv().await {
                        data_channel.send(&message.into()).await.unwrap();
                    }
                });
            });
            Self {
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
            let (signal_sender, signal_to_thread) = mpsc::channel::<Signal>();
            let (signal_from_thread, signal_receiver) = mpsc::channel::<Signal>();
            let (channel_sender, channel_receiver) = mpsc::channel::<DataChannel>();
            let wake_up = Arc::new(RwLock::new(None));
            let wake_up_cloned = wake_up.clone();
            spawn_local(async move {
                let mut closures = vec![];
                let signal_receiver = signal_to_thread;
                let signal_sender = signal_from_thread;
                let wake_up = wake_up_cloned;
                let mut configuration = RtcConfiguration::new();
                let ice_servers = Array::new();
                let mut ice_server = Object::new();
                let urls = Array::new();
                urls.push(&JsValue::from("stun:stun.l.google.com:19302"));
                Reflect::set(&mut ice_server, &JsValue::from("urls"), &urls).unwrap();
                configuration.ice_servers(&ice_servers);
                configuration.ice_transport_policy(RtcIceTransportPolicy::All);
                let peer = RtcPeerConnection::new_with_configuration(&configuration).unwrap();
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
                    let onopen = Closure::wrap(Box::new(move |event: JsValue| {
                        let channel = RtcDataChannel::from(
                            Reflect::get(&event, &JsValue::from("target")).unwrap(),
                        );
                        let (obj_to_rtc_sender, obj_to_rtc_receiver) = mpsc::channel::<Vec<u8>>();
                        let (rtc_to_obj_sender, rtc_to_obj_receiver) = mpsc::channel::<Vec<u8>>();
                        *open_data_channel.write().unwrap() = Some((channel.clone(), obj_to_rtc_receiver));
                        channel_sender
                            .send(DataChannel {
                                wake_up: wake_up.clone(),
                                message_sender: obj_to_rtc_sender,
                                message_receiver: rtc_to_obj_receiver,
                            })
                            .unwrap();
                        let message_sender = rtc_to_obj_sender;
                        let onmessage = Closure::wrap(Box::new(move |event: JsValue| {
                            let data = Reflect::get(&event, &"data".into()).unwrap();
                            let is_string = data.is_string();
                            let bytes = if is_string {
                                let encoder = TextEncoder::new().unwrap();
                                encoder.encode_with_input(data.as_string().unwrap().as_str())
                            } else {
                                Uint8Array::new(&data).to_vec()
                            };
                            message_sender.send(bytes).unwrap();
                        })
                            as Box<dyn Fn(JsValue)>);
                        channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
                        onmessage.forget(); // TODO: don't forget
                    }) as Box<dyn Fn(JsValue)>);
                    data_channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));
                    closures.push(onopen);
                    let offer = RtcSessionDescription::from(
                        JsFuture::from(peer.create_offer()).await.unwrap(),
                    );
                    let mut offer_init = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                    offer_init.sdp(&offer.sdp());
                    JsFuture::from(peer.set_local_description(&offer_init))
                        .await
                        .unwrap();
                    signal_sender.send(Signal::Offer(offer.sdp())).unwrap();
                } else {
                    let open_data_channel = open_data_channel.clone();
                    let wake_up = wake_up.clone();
                    let ondatachannel = Closure::wrap(Box::new(move |event: JsValue| {
                        let channel = js_sys::Reflect::get(&event, &"channel".into()).unwrap();
                        let data_channel = RtcDataChannel::from(channel);
                        data_channel.set_binary_type(RtcDataChannelType::Arraybuffer);
                        let open_data_channel = open_data_channel.clone();
                        let channel_sender = channel_sender.clone();
                        let wake_up = wake_up.clone();
                        let onopen = Closure::wrap(Box::new(move |event: JsValue| {
                            let channel = RtcDataChannel::from(
                                Reflect::get(&event, &JsValue::from("target")).unwrap(),
                            );
                            let (obj_to_rtc_sender, obj_to_rtc_receiver) =
                                mpsc::channel::<Vec<u8>>();
                            let (rtc_to_obj_sender, rtc_to_obj_receiver) =
                                mpsc::channel::<Vec<u8>>();
                            *open_data_channel.write().unwrap() =
                                Some((channel.clone(), obj_to_rtc_receiver));
                            channel_sender
                                .send(DataChannel {
                                    wake_up: wake_up.clone(),
                                    message_sender: obj_to_rtc_sender,
                                    message_receiver: rtc_to_obj_receiver,
                                })
                                .unwrap();
                            let message_sender = rtc_to_obj_sender;
                            let onmessage = Closure::wrap(Box::new(move |event: JsValue| {
                                let data = Reflect::get(&event, &"data".into()).unwrap();
                                let is_string = data.is_string();
                                let bytes = if is_string {
                                    let encoder = TextEncoder::new().unwrap();
                                    encoder.encode_with_input(data.as_string().unwrap().as_str())
                                } else {
                                    Uint8Array::new(&data).to_vec()
                                };
                                message_sender.send(bytes).unwrap();
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
                        let candidate = Reflect::get(&ice_candidate, &"candidate".into()).unwrap();
                        signal_sender
                            .send(Signal::IceCandidate(if candidate.is_object() {
                                Some(
                                    Reflect::get(&candidate, &"candidate".into())
                                        .unwrap()
                                        .as_string()
                                        .unwrap(),
                                )
                            } else {
                                None
                            }))
                            .unwrap();
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
                                JsFuture::from(peer.set_remote_description(&offer_init))
                                    .await
                                    .unwrap();
                                let answer = RtcSessionDescription::from(
                                    JsFuture::from(peer.create_answer()).await.unwrap(),
                                );
                                let mut answer_init =
                                    RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                                answer_init.sdp(&answer.sdp());
                                JsFuture::from(peer.set_local_description(&answer_init))
                                    .await
                                    .unwrap();
                                signal_sender.send(Signal::Answer(answer.sdp())).unwrap();
                            }
                            Signal::Answer(answer) => {
                                let mut answer_init =
                                    RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                                answer_init.sdp(&answer);
                                JsFuture::from(peer.set_remote_description(&answer_init))
                                    .await
                                    .unwrap();
                            }
                            Signal::IceCandidate(ice_candidate) => {
                                if let Some(ice_candidate) = ice_candidate {
                                    let mut ice_candidate_init =
                                        RtcIceCandidateInit::new(&ice_candidate);
                                    ice_candidate_init.sdp_m_line_index(Some(0));
                                    ice_candidate_init.sdp_mid(Some("0"));
                                    JsFuture::from(
                                        peer.add_ice_candidate_with_opt_rtc_ice_candidate_init(
                                            Some(&ice_candidate_init),
                                        ),
                                    )
                                    .await
                                    .unwrap();
                                } else {
                                    JsFuture::from(
                                        peer.add_ice_candidate_with_opt_rtc_ice_candidate_init(
                                            None,
                                        ),
                                    )
                                    .await
                                    .unwrap();
                                }
                            }
                        },
                        Err(TryRecvError::Empty) => {
                            let promise = Promise::new(&mut |resolve, _reject| {
                                *wake_up.write().unwrap() = Some(resolve);
                            });
                            JsFuture::from(promise).await.unwrap();
                        }
                        Err(TryRecvError::Disconnected) => {
                            break;
                        }
                    }
                }
                let (data_channel, message_receiver) =
                    open_data_channel.write().unwrap().take().unwrap();
                // TODO: onclose
                loop {
                    match message_receiver.try_recv() {
                        Ok(message) => {
                            data_channel.send_with_u8_array(&message).unwrap();
                        }
                        Err(TryRecvError::Empty) => {
                            let promise = Promise::new(&mut |resolve, _reject| {
                                *wake_up.write().unwrap() = Some(resolve);
                            });
                            JsFuture::from(promise).await.unwrap();
                        }
                        Err(TryRecvError::Disconnected) => {
                            break;
                        }
                    }
                }
            });
            Self {
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
            self.signal_receiver.try_recv().ok() // TODO: handle errors
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.signal_receiver.try_recv().ok() // TODO: handle errors
        }
    }

    pub fn receive(&mut self, signal: Signal) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(signal_sender) = self.signal_sender.as_mut() {
                tokio::task::block_in_place(|| {
                    signal_sender.blocking_send(signal).unwrap();
                });
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsValue;
            if let Some(signal_sender) = self.signal_sender.as_mut() {
                signal_sender.send(signal).unwrap();
            }
            if let Some(wake_up) = self.wake_up.write().unwrap().as_mut().take() {
                wake_up.call0(&JsValue::null()).unwrap();
            }
        }
    }

    pub fn data_channel(&mut self) -> Option<DataChannel> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(data_channel) = self.channel_receiver.try_recv().ok() {
                self.signal_sender = None;
                Some(data_channel)
            } else {
                None
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsValue;
            if let Some(data_channel) = self.channel_receiver.try_recv().ok() {
                self.signal_sender = None;
                if let Some(wake_up) = self.wake_up.write().unwrap().as_mut().take() {
                    wake_up.call0(&JsValue::null()).unwrap();
                }
                Some(data_channel)
            } else {
                None
            }
        }
    }
}

pub struct DataChannel {
    #[cfg(not(target_arch = "wasm32"))]
    message_sender: tokio::sync::mpsc::Sender<Vec<u8>>,
    #[cfg(not(target_arch = "wasm32"))]
    message_receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
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
                self.message_sender.blocking_send(message).unwrap();
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsValue;
            self.message_sender.send(message).unwrap();
            if let Some(wake_up) = self.wake_up.write().unwrap().as_mut().take() {
                wake_up.call0(&JsValue::null()).unwrap();
            }
        }
    }

    pub fn receive(&mut self) -> Option<Vec<u8>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.message_receiver.try_recv().ok() // TODO
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.message_receiver.try_recv().ok() // TODO
        }
    }
}
