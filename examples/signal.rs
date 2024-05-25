use std::{str, time::Duration};

use simple_webrtc_channel::{DataChannel, DataChannelConfiguration, Signaler, SignalerKind};

fn main() {
    let mut offerer = Signaler::new(SignalerKind::Offer {
        data_channel_configuration: DataChannelConfiguration::Reliable,
    });
    let mut offerer_data_channel = None;

    let mut answerer = Signaler::new(SignalerKind::Answer);
    let mut answerer_data_channel = None;

    loop {
        update(
            "Offerer",
            &mut offerer,
            &mut offerer_data_channel,
            &mut answerer,
        );
        update(
            "Answerer",
            &mut answerer,
            &mut answerer_data_channel,
            &mut offerer,
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn update(
    name: &str,
    me: &mut Signaler,
    my_data_channel: &mut Option<DataChannel>,
    them: &mut Signaler,
) {
    while let Some(signal) = me.signal() {
        them.receive(signal);
    }
    if let Some(mut data_channel) = me.data_channel().unwrap() {
        println!("[{}] Connected!", name);
        if name == "Offerer" {
            data_channel.send("Hello world".as_bytes().to_vec());
        }
        *my_data_channel = Some(data_channel);
    }
    if let Some(data_channel) = my_data_channel {
        while let Some(message) = data_channel.receive().unwrap() {
            println!(
                "[{}] Received message: {}",
                name,
                str::from_utf8(&message).unwrap()
            );
        }
    }
}
