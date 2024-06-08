use std::{net::SocketAddr, str, time::Duration};

use local_ip_address::local_ip;
use simple_webrtc_channel::{Client, Configuration, DataChannelConfiguration, IceServer, Server};

pub fn webrtc_configuration() -> Configuration {
    Configuration {
        ice_servers: vec![IceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn main() {
    std::thread::spawn(|| {
        let mut server = Server::new(
            SocketAddr::from(([0, 0, 0, 0], 3000)),
            Some(SocketAddr::from(([0, 0, 0, 0], 3001))),
            Some(vec![local_ip().unwrap().to_string()]),
            webrtc_configuration(),
        )
        .unwrap();
        let mut data_channels = vec![];
        loop {
            if let Some(data_channel) = server.accept() {
                println!("[SERVER] Accepted connection!");
                data_channels.push(data_channel);
            }
            for data_channel in &mut data_channels {
                while let Some(message) = data_channel.receive().unwrap() {
                    println!(
                        "[SERVER] Received message: {}",
                        str::from_utf8(&message).unwrap()
                    );
                    data_channel.send(message);
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    std::thread::sleep(Duration::from_millis(200));
    let mut client = Client::new(
        "http://127.0.0.1:3000",
        webrtc_configuration(),
        DataChannelConfiguration::Reliable,
    );
    let mut data_channel = loop {
        if let Some(data_channel) = client.check_connection().unwrap() {
            break data_channel;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    data_channel.send("Hello world!".as_bytes().to_vec());
    println!("[CLIENT] Connected!");
    loop {
        while let Some(message) = data_channel.receive().unwrap() {
            println!(
                "[CLIENT] Received message: {}",
                str::from_utf8(&message).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
