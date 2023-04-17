# blockchain-workshop
This is a simple blockchain featuring a local p2p network, simple mempool and concensus for educational purposes.

# Step 1

In this step we will create a p2p network utilizing libp2p library

We will connect to other peers in the network by using the `MDNS` protocol.

Message will be entered via `stdin` and exchanged by utilizing `Gossipsub` protocol.

If we were successful we should be able to send one message over the network and everyone should receive it before getting
an error that the message is duplicate. 