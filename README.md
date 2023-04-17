# blockchain-workshop
This is a simple blockchain featuring a local p2p network, simple mempool and concensus for educational purposes.

# Step 4

In this step we will validate transactions after we collected 10 in our mempool.

Upon successful validation the node will cast its vote over `Gossipsub` on a separate topic.

When votes from all nodes are received we will create a block.