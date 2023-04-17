use async_std::io;
use futures::{prelude::*, select};
use libp2p::{
    core::upgrade,
    gossipsub,
    identity::{self},
    mdns, noise,
    swarm::NetworkBehaviour,
    swarm::{SwarmBuilder, SwarmEvent},
    tcp, yamux, PeerId, Transport,
};
use std::error::Error;
use std::time::Duration;

// We create a custom network behaviour that combines Gossipsub and Mdns.
#[derive(NetworkBehaviour)]
struct EduCoinBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::async_io::Behaviour,
}

#[async_std::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Create a random PeerId
    let id_keys = identity::Keypair::generate_ed25519();
    let local_peer_id = PeerId::from(id_keys.public());
    println!("Local peer id: {local_peer_id}");

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let tcp_transport = tcp::async_io::Transport::new(tcp::Config::default().nodelay(true))
        .upgrade(upgrade::Version::V1)
        .authenticate(
            noise::NoiseAuthenticated::xx(&id_keys).expect("signing libp2p-noise static keypair"),
        )
        .multiplex(yamux::YamuxConfig::default())
        .timeout(std::time::Duration::from_secs(20))
        .boxed();

    let keypair_clone = id_keys.clone();

    // Message signing function
    let message_id_fn = move |message: &gossipsub::Message| {
        // We take our private key and sign the message
        // The resulting signature is in bytes
        let message_signature = keypair_clone
            .sign(&message.data)
            .expect("failed signing message data");

        // Serialize the public key into bytes
        // The public key is 32 bytes long
        let public_key = keypair_clone
            .public()
            .into_ed25519()
            .unwrap()
            .encode()
            .to_vec();

        let pub_key_and_sig = [public_key, message_signature].concat();
        gossipsub::MessageId::from(pub_key_and_sig)
    };

    // Set a custom gossipsub configuration
    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(10)) // This is set to aid debugging by not cluttering the log space
        .validation_mode(gossipsub::ValidationMode::Strict) // This sets the kind of message validation. The default is Strict (enforce message signing)
        .message_id_fn(message_id_fn) // content-address messages. No two messages of the same content will be propagated.
        .build()
        .expect("Valid config");

    // build a gossipsub network behaviour
    let mut gossipsub = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(id_keys.clone()),
        gossipsub_config,
    )
    .expect("Correct configuration");
    // Create a Gossipsub topic over which we will send transactions
    let transactions_topic = gossipsub::IdentTopic::new("transaction");
    // subscribes to our topic
    gossipsub.subscribe(&transactions_topic)?;

    // Create a Swarm to manage peers and events
    let mut swarm = {
        let mdns = mdns::async_io::Behaviour::new(mdns::Config::default(), local_peer_id)?;
        let behaviour = EduCoinBehaviour { gossipsub, mdns };
        SwarmBuilder::with_async_std_executor(tcp_transport, behaviour, local_peer_id).build()
    };

    // Read full lines from stdin
    let mut stdin = io::BufReader::new(io::stdin()).lines().fuse();

    // Listen on all interfaces and whatever port the OS assigns
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    println!("Enter messages via STDIN and they will be sent to connected peers using Gossipsub");

    // Kick it off
    loop {
        select! {
            line = stdin.select_next_some() => {
                let read_line = line.expect("Stdin not to close");

                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(
                    transactions_topic.clone(),
                    read_line.as_bytes(),
                ) {
                    println!("Publish error: {e:?}");
                }
            },
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(EduCoinBehaviourEvent::Mdns(mdns::Event::Discovered(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("------> mDNS discovered a new peer: {peer_id}");
                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(EduCoinBehaviourEvent::Mdns(mdns::Event::Expired(list))) => {
                    for (peer_id, _multiaddr) in list {
                        println!("------> mDNS discover peer has expired: {peer_id}");
                        swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                    }
                },
                SwarmEvent::Behaviour(EduCoinBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    propagation_source: _peer_id,
                    message_id: _id,
                    message,
                })) => {
                    println!("{}", String::from_utf8_lossy(&message.data));
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
}
