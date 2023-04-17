use async_std::io;
use futures::{prelude::*, select};
use libp2p::{
    core::upgrade,
    gossipsub,
    identity::{self, ed25519, ed25519::PublicKey},
    mdns, noise,
    swarm::NetworkBehaviour,
    swarm::{SwarmBuilder, SwarmEvent},
    tcp, yamux, PeerId, Transport,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::{error::Error, sync::Mutex};
use std::{fs, time::Duration};

// We create a custom network behaviour that combines Gossipsub and Mdns.
#[derive(NetworkBehaviour)]
struct EduCoinBehaviour {
    gossipsub: gossipsub::Behaviour,
    mdns: mdns::async_io::Behaviour,
}

#[derive(Debug)]
struct Transaction {
    pub public_key: PublicKey,
    pub signature: Vec<u8>,
    pub data: Vec<u8>,
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

    // To content-address message, we can take the hash of message and use it as an ID.
    let message_id_fn = move |message: &gossipsub::Message| {
        let message_signature = keypair_clone
            .sign(&message.data)
            .expect("failed signing message data");

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
    let transaction_topic_hash = transactions_topic.hash();

    // Create a topic over which we will notify nodes to start validating transactions
    let vote_topic = gossipsub::IdentTopic::new("vote");
    gossipsub.subscribe(&vote_topic)?;
    let vote_topic_hash = vote_topic.hash();

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

    let mempool = Arc::new(Mutex::new(Vec::<Transaction>::new()));
    let mut block_height = 1u32;

    let voters = Arc::new(Mutex::new(HashSet::<PeerId>::new()));

    // Kick it off
    loop {
        let number_of_peers = swarm.behaviour_mut().gossipsub.all_peers().count();

        // consensus was reached clear the voters for current block height
        if voters.lock().unwrap().len() == number_of_peers && number_of_peers > 0 {
            println!("------> collected all of the votes executing consensus");
            println!("------> taking first 10 transactions from mempool");
            let first_ten_trx = mempool
                .lock()
                .unwrap()
                .drain(..10)
                .collect::<Vec<Transaction>>();

            println!("-----> writing block to the disk");
            let file_name = format!("block_{block_height}.txt");
            let file_contents = format!("{first_ten_trx:?}");
            fs::write(file_name, file_contents).unwrap();

            println!("----> clearing votes collected for the current block");
            voters.clone().lock().unwrap().clear();

            block_height += 1;
        }

        select! {
            line = stdin.select_next_some() => {
                println!("------> Transaction received on node: storing into local mempool and publishing");
                let read_line = line.expect("Stdin not to close");

                let typed_keypair = id_keys.clone().into_ed25519().unwrap();

                mempool.lock().unwrap().push(Transaction {
                    public_key: typed_keypair.public(),
                    data: read_line.clone().as_bytes().to_vec(),
                    signature: typed_keypair.sign(read_line.clone().as_bytes())
                });

                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(
                    transactions_topic.clone(),
                    read_line.as_bytes(),
                ) {
                    println!("Publish error: {e:?}");
                }

                if mempool.lock().unwrap().len() == 10 {
                    println!("-----> 10 transactions collected on local node, validating...");
                    let mut correct_transaction_num: u32 = 0;

                    for transaction in mempool.lock().unwrap().iter() {
                        let is_valid = transaction.public_key.verify(&transaction.data, &transaction.signature);

                        if is_valid {
                            correct_transaction_num += 1;
                        }
                    }

                    let peer_id_string = local_peer_id.to_string();
                    let vote_message = format!("{block_height}{peer_id_string}");
                    println!("----> vote message: {vote_message}");

                    // Since gossipsub doesn't like duplicated messages we're going to make our messages
                    // unique by adding our node peer_id to the block height
                    if correct_transaction_num == 10 {
                        println!("------> all transactions are valid sending vote");

                        if let Err(e) = swarm
                            .behaviour_mut()
                            .gossipsub
                            .publish(vote_topic.clone(), vote_message.as_bytes()) {
                                println!("Publish error when casting vote: {e:?}");
                            }
                    }
                }

                println!("------> Transaction stored and published");
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
                    propagation_source: peer_id,
                    message_id: id,
                    message,
                })) => {
                    println!("------> got a new message, processing....");
                    // remember that first 32 bytes are the public key
                    let (public_key_bytes, signature) = id.0.split_at(32);

                    // create a buffer to copy the bytes of the public key
                    let mut public_key: [u8; 32] = [0; 32];
                    public_key.copy_from_slice(public_key_bytes);
                    // reconstruct the public key from bytes received
                    let public_key = ed25519::PublicKey::decode(&public_key).unwrap();

                    // handle consensus votes
                    if message.topic == vote_topic_hash {
                        println!("------> got a vote, storing the voter");
                        if public_key.verify(&message.data, &signature) {
                            voters.clone().lock().unwrap().insert(peer_id);
                        }
                    }

                    if message.topic == transaction_topic_hash {
                        println!("------> got a new transactions, storing into mempool");
                        // create a transaction struct and push it into mempool
                        let transaction = Transaction {
                            public_key,
                            signature: signature.to_vec(),
                            data: message.data
                        };
                        mempool.lock().unwrap().push(transaction);

                        let mempool_len = mempool.lock().unwrap().len();
                        println!("-----> num of transactions in mempool: {mempool_len}");

                        // oh no, duplicated code! :D
                        if mempool.lock().unwrap().len() == 10 {
                            println!("------> collected 10 transactions, validating...");
                            let mut correct_transaction_num: u32 = 0;

                            for transaction in mempool.lock().unwrap().iter() {
                                let is_valid = transaction.public_key.verify(&transaction.data, &transaction.signature);

                                if is_valid {
                                    correct_transaction_num += 1;
                                }
                            }

                            // Since gossipsub doesn't like duplicated messages we're going to make our messages
                            // unique by adding our node peer_id to the block height
                            let peer_id_string = local_peer_id.to_string();
                            let vote_message = format!("{block_height}{peer_id_string}");

                            if correct_transaction_num == 10 {
                                println!("------> all transactions are valid sending vote");

                                if let Err(e) = swarm
                                    .behaviour_mut()
                                    .gossipsub
                                    .publish(vote_topic.clone(), vote_message.as_bytes()) {
                                        println!("Publish error when casting vote: {e:?}");
                                    }
                            }
                        }

                        println!("{mempool:?}");
                    }
                },
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Local node is listening on {address}");
                }
                _ => {}
            }
        }
    }
}
